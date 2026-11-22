// SPDX-License-Identifier: AGPL-3.0-or-later
#import <AVFoundation/AVFoundation.h>
#import <CoreVideo/CoreVideo.h>
#import <Foundation/Foundation.h>
#include <stdbool.h>

typedef void (*DeviceCallback)(void *, const char *, const char *);
typedef void (*PermissionCallback)(void *, bool);
typedef void (*FrameCallback)(void *, const uint8_t *, size_t, const uint8_t *, size_t,
                              size_t, size_t);

@interface FluxerCameraCapture : NSObject <AVCaptureVideoDataOutputSampleBufferDelegate>
@property(nonatomic, strong) AVCaptureSession *session;
@property(nonatomic, strong) AVCaptureVideoDataOutput *output;
@property(nonatomic, strong) dispatch_queue_t queue;
@property(nonatomic, strong) NSLock *lock;
@property(nonatomic, assign) CVPixelBufferRef latestFrame;
@end

@implementation FluxerCameraCapture
- (void)captureOutput:(AVCaptureOutput *)output
    didOutputSampleBuffer:(CMSampleBufferRef)sample
           fromConnection:(AVCaptureConnection *)connection {
    (void)output;
    (void)connection;
    CVPixelBufferRef frame = CMSampleBufferGetImageBuffer(sample);
    if (frame == NULL) {
        return;
    }
    // Keep one retained image. The consumer can fall behind without building a
    // queue of private frames or preventing AVFoundation from reusing buffers.
    CVPixelBufferRetain(frame);
    [self.lock lock];
    CVPixelBufferRef previous = self.latestFrame;
    self.latestFrame = frame;
    [self.lock unlock];
    if (previous != NULL) {
        CVPixelBufferRelease(previous);
    }
}

- (void)dealloc {
    if (_latestFrame != NULL) {
        CVPixelBufferRelease(_latestFrame);
    }
}
@end

void fluxer_camera_devices(void *context, DeviceCallback callback) {
    @autoreleasepool {
        if (@available(macOS 10.15, *)) {
            NSArray<AVCaptureDeviceType> *types;
            if (@available(macOS 14.0, *)) {
                types = @[AVCaptureDeviceTypeBuiltInWideAngleCamera, AVCaptureDeviceTypeExternal,
                          AVCaptureDeviceTypeContinuityCamera];
            } else {
                // The earlier name is required on macOS 13 and below.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
                types = @[AVCaptureDeviceTypeBuiltInWideAngleCamera,
                          AVCaptureDeviceTypeExternalUnknown];
#pragma clang diagnostic pop
            }
            AVCaptureDeviceDiscoverySession *discovery = [AVCaptureDeviceDiscoverySession
                discoverySessionWithDeviceTypes:types
                                      mediaType:AVMediaTypeVideo
                                       position:AVCaptureDevicePositionUnspecified];
            for (AVCaptureDevice *device in discovery.devices) {
                callback(context, device.uniqueID.UTF8String, device.localizedName.UTF8String);
            }
        }
    }
}

int fluxer_camera_authorization(void) {
    @autoreleasepool {
        if ([[NSBundle mainBundle] objectForInfoDictionaryKey:@"NSCameraUsageDescription"] == nil) {
            return -1;
        }
        return (int)[AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeVideo];
    }
}

void fluxer_camera_request_access(void *context, PermissionCallback callback) {
    @autoreleasepool {
        [AVCaptureDevice requestAccessForMediaType:AVMediaTypeVideo
                               completionHandler:^(BOOL granted) {
            callback(context, granted);
        }];
    }
}

void *fluxer_camera_open(const char *device_id) {
    @autoreleasepool {
        NSString *uid = [NSString stringWithUTF8String:device_id];
        AVCaptureDevice *device = uid ? [AVCaptureDevice deviceWithUniqueID:uid] : nil;
        if (device == nil || ![device hasMediaType:AVMediaTypeVideo]) {
            return NULL;
        }
        AVCaptureDeviceInput *input = [AVCaptureDeviceInput deviceInputWithDevice:device error:NULL];
        if (input == nil) {
            return NULL;
        }

        FluxerCameraCapture *capture = [[FluxerCameraCapture alloc] init];
        capture.session = [[AVCaptureSession alloc] init];
        capture.output = [[AVCaptureVideoDataOutput alloc] init];
        capture.queue = dispatch_queue_create("app.fluxer.Native.camera.frames", DISPATCH_QUEUE_SERIAL);
        capture.lock = [[NSLock alloc] init];
        capture.output.alwaysDiscardsLateVideoFrames = YES;

        [capture.session beginConfiguration];
        if (![capture.session canAddInput:input] ||
            ![capture.session canAddOutput:capture.output]) {
            [capture.session commitConfiguration];
            return NULL;
        }
        [capture.session addInput:input];
        [capture.session addOutput:capture.output];
        if ([capture.session canSetSessionPreset:AVCaptureSessionPreset640x480]) {
            capture.session.sessionPreset = AVCaptureSessionPreset640x480;
        }
        NSNumber *format = @(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
        if (![capture.output.availableVideoCVPixelFormatTypes containsObject:format]) {
            [capture.session commitConfiguration];
            return NULL;
        }
        capture.output.videoSettings = @{(id)kCVPixelBufferPixelFormatTypeKey: format};
        [capture.output setSampleBufferDelegate:capture queue:capture.queue];
        [capture.session commitConfiguration];
        [capture.session startRunning];
        if (!capture.session.isRunning) {
            [capture.output setSampleBufferDelegate:nil queue:NULL];
            dispatch_sync(capture.queue, ^{});
            return NULL;
        }
        return (__bridge_retained void *)capture;
    }
}

void fluxer_camera_read(void *handle, void *context, FrameCallback callback) {
    @autoreleasepool {
        FluxerCameraCapture *capture = (__bridge FluxerCameraCapture *)handle;
        [capture.lock lock];
        CVPixelBufferRef frame = capture.latestFrame;
        capture.latestFrame = NULL;
        [capture.lock unlock];
        if (frame == NULL) {
            return;
        }
        if (CVPixelBufferGetPixelFormatType(frame) == kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange &&
            CVPixelBufferGetPlaneCount(frame) == 2 &&
            CVPixelBufferLockBaseAddress(frame, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess) {
            callback(context, CVPixelBufferGetBaseAddressOfPlane(frame, 0),
                     CVPixelBufferGetBytesPerRowOfPlane(frame, 0),
                     CVPixelBufferGetBaseAddressOfPlane(frame, 1),
                     CVPixelBufferGetBytesPerRowOfPlane(frame, 1),
                     CVPixelBufferGetWidth(frame), CVPixelBufferGetHeight(frame));
            CVPixelBufferUnlockBaseAddress(frame, kCVPixelBufferLock_ReadOnly);
        }
        CVPixelBufferRelease(frame);
    }
}

void fluxer_camera_close(void *handle) {
    @autoreleasepool {
        FluxerCameraCapture *capture = (__bridge_transfer FluxerCameraCapture *)handle;
        [capture.output setSampleBufferDelegate:nil queue:NULL];
        [capture.session stopRunning];
        dispatch_sync(capture.queue, ^{});
    }
}
