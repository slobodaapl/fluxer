// SPDX-License-Identifier: AGPL-3.0-or-later
#include <libavformat/avformat.h>
#include <libavcodec/avcodec.h>
#include <libavutil/channel_layout.h>
#include <libavutil/error.h>
#include <libavutil/imgutils.h>
#include <libswscale/swscale.h>
#include <libswresample/swresample.h>
#include <stdlib.h>
#include <string.h>

// All handles are owned by one Rust playback worker.
typedef struct {
    AVFormatContext *format;
    AVCodecContext *video, *audio;
    int vi, ai, draining, active, eof, flush_video, flush_audio;
    AVPacket *packet;
    AVFrame *frame;
    struct SwsContext *scale;
    SwrContext *resample;
    enum AVSampleFormat audio_format;
    int audio_rate;
    AVChannelLayout audio_layout;
    double origin;
} FluxDecoder;
typedef struct { int kind, width, height; double pts, duration; unsigned char *data; size_t len; } FluxDecoded;
static void error_text(char *out, size_t len, int err) { if (av_strerror(err, out, len) < 0) snprintf(out, len, "FFmpeg error %d", err); }
static AVCodecContext *open_codec(AVFormatContext *f, int index, char *error, size_t len) {
    if (index < 0) return NULL;
    AVStream *s = f->streams[index]; const AVCodec *codec = avcodec_find_decoder(s->codecpar->codec_id);
    if (!codec) { snprintf(error, len, "Required media decoder is unavailable"); return NULL; }
    AVCodecContext *ctx = avcodec_alloc_context3(codec);
    if (!ctx) { snprintf(error, len, "Out of memory"); return NULL; }
    int r = avcodec_parameters_to_context(ctx, s->codecpar);
    if (s->codecpar->codec_type == AVMEDIA_TYPE_VIDEO) {
        // Bound decoder surfaces and thread-local copies even for hostile files.
        ctx->max_pixels = 4096LL * 2304;
        ctx->thread_count = 4;
    }
    if (r >= 0) r = avcodec_open2(ctx, codec, NULL);
    if (r < 0) { error_text(error, len, r); avcodec_free_context(&ctx); }
    return ctx;
}
void flux_decoder_free(FluxDecoder *d) {
    if (!d) return;
    avcodec_free_context(&d->video); avcodec_free_context(&d->audio);
    av_packet_free(&d->packet); av_frame_free(&d->frame); avformat_close_input(&d->format);
    sws_freeContext(d->scale); swr_free(&d->resample); av_channel_layout_uninit(&d->audio_layout); free(d);
}
FluxDecoder *flux_decoder_open(const char *path, char *error, size_t len) {
    FluxDecoder *d = calloc(1, sizeof(*d)); if (!d) { snprintf(error, len, "Out of memory"); return NULL; }
    AVDictionary *opts = NULL;
    av_dict_set(&opts, "protocol_whitelist", "file", 0);
    av_dict_set(&opts, "format_whitelist", "mov,matroska,webm,mp3,ogg,wav,flac,aac,mpegts,mpeg,avi,gif,asf,ape,opus", 0);
    int r = avformat_open_input(&d->format, path, NULL, &opts);
    av_dict_free(&opts);
    if (r >= 0) r = avformat_find_stream_info(d->format, NULL);
    if (r < 0) { error_text(error, len, r); goto fail; }
    d->vi = av_find_best_stream(d->format, AVMEDIA_TYPE_VIDEO, -1, -1, NULL, 0);
    d->ai = av_find_best_stream(d->format, AVMEDIA_TYPE_AUDIO, -1, -1, NULL, 0);
    if (d->vi < 0 && d->ai < 0) { snprintf(error, len, "File contains no playable audio or video"); goto fail; }
    if (d->vi >= 0 && !(d->video = open_codec(d->format, d->vi, error, len))) goto fail;
    if (d->ai >= 0 && !(d->audio = open_codec(d->format, d->ai, error, len))) goto fail;
    d->packet = av_packet_alloc(); d->frame = av_frame_alloc();
    if (!d->packet || !d->frame) { snprintf(error, len, "Out of memory"); goto fail; }
    d->active = -1;
    d->origin = d->format->start_time == AV_NOPTS_VALUE ? 0 : (double)d->format->start_time / AV_TIME_BASE;
    return d;
fail: flux_decoder_free(d); return NULL;
}
int flux_decoder_has_audio(FluxDecoder *d) { return d->audio != NULL; }
double flux_decoder_duration(FluxDecoder *d) { return d->format->duration == AV_NOPTS_VALUE ? -1 : (double)d->format->duration / AV_TIME_BASE; }
int flux_decoder_seek(FluxDecoder *d, double seconds, char *error, size_t len) {
    int r = avformat_seek_file(d->format, -1, INT64_MIN, (int64_t)((seconds + d->origin) * AV_TIME_BASE), INT64_MAX, AVSEEK_FLAG_BACKWARD);
    if (r < 0) { error_text(error, len, r); return 0; }
    if (d->video) avcodec_flush_buffers(d->video);
    if (d->audio) avcodec_flush_buffers(d->audio);
    swr_free(&d->resample); av_channel_layout_uninit(&d->audio_layout);
    d->active = -1; d->eof = d->flush_audio = d->flush_video = 0; av_packet_unref(d->packet); return 1;
}
static int convert_frame(FluxDecoder *d, int stream, FluxDecoded *out, char *error, size_t len) {
    AVFrame *f = d->frame; AVStream *s = d->format->streams[stream];
    int64_t ts = f->best_effort_timestamp;
    out->pts = ts == AV_NOPTS_VALUE ? -1 : (double)ts * av_q2d(s->time_base) - d->origin;
    out->duration = f->duration > 0 ? (double)f->duration * av_q2d(s->time_base) : 0;
    if (stream == d->vi) {
        if (f->width <= 0 || f->height <= 0 || (int64_t)f->width * f->height > 4096LL * 2304) { snprintf(error, len, "Invalid video dimensions"); return -1; }
        d->scale = sws_getCachedContext(d->scale, f->width, f->height, f->format, f->width, f->height, AV_PIX_FMT_BGRA, SWS_BILINEAR, NULL, NULL, NULL);
        if (!d->scale) { snprintf(error, len, "Unable to convert video"); return -1; }
        out->kind = 1; out->width = f->width; out->height = f->height; out->len = (size_t)f->width * f->height * 4;
        out->data = malloc(out->len); if (!out->data) { snprintf(error, len, "Out of memory"); return -1; }
        uint8_t *dst[] = {out->data, NULL, NULL, NULL}; int stride[] = { f->width * 4, 0, 0, 0 };
        sws_scale(d->scale, (const uint8_t *const *)f->data, f->linesize, 0, f->height, dst, stride);
        return 1;
    }
    if (!d->resample || d->audio_rate != f->sample_rate || d->audio_format != f->format || av_channel_layout_compare(&d->audio_layout, &f->ch_layout)) {
        swr_free(&d->resample); av_channel_layout_uninit(&d->audio_layout);
        AVChannelLayout stereo = AV_CHANNEL_LAYOUT_STEREO;
        int r = swr_alloc_set_opts2(&d->resample, &stereo, AV_SAMPLE_FMT_FLT, 48000, &f->ch_layout, f->format, f->sample_rate, 0, NULL);
        if (r >= 0) r = swr_init(d->resample);
        if (r < 0) { error_text(error, len, r); return -1; }
        d->audio_rate = f->sample_rate; d->audio_format = f->format; av_channel_layout_copy(&d->audio_layout, &f->ch_layout);
    }
    int count = swr_get_out_samples(d->resample, f->nb_samples);
    if (count < 0 || count > 480000) { snprintf(error, len, "Invalid decoded audio size"); return -1; }
    out->data = malloc((size_t)count * 2 * sizeof(float)); if (!out->data) { snprintf(error, len, "Out of memory"); return -1; }
    uint8_t *dst[] = {out->data}; int r = swr_convert(d->resample, dst, count, (const uint8_t **)f->extended_data, f->nb_samples);
    if (r < 0) { free(out->data); out->data = NULL; error_text(error, len, r); return -1; }
    out->kind = 2; out->len = (size_t)r * 2 * sizeof(float); return 1;
}
// 1 decoded frame, 0 end of stream, -1 error.
int flux_decoder_next(FluxDecoder *d, FluxDecoded *out, char *error, size_t len) {
    memset(out, 0, sizeof(*out));
    for (;;) {
        if (d->active >= 0) {
            AVCodecContext *ctx = d->active == d->vi ? d->video : d->audio;
            int r = avcodec_receive_frame(ctx, d->frame);
            if (r >= 0) { int result = convert_frame(d, d->active, out, error, len); av_frame_unref(d->frame); return result; }
            if (r != AVERROR(EAGAIN) && r != AVERROR_EOF) { error_text(error, len, r); return -1; }
            d->active = -1;
        }
        if (d->eof) {
            if (d->video && !d->flush_video) { d->flush_video = 1; avcodec_send_packet(d->video, NULL); d->active = d->vi; continue; }
            if (d->audio && !d->flush_audio) { d->flush_audio = 1; avcodec_send_packet(d->audio, NULL); d->active = d->ai; continue; }
            return 0;
        }
        int r = av_read_frame(d->format, d->packet);
        if (r == AVERROR_EOF) { d->eof = 1; continue; }
        if (r < 0) { error_text(error, len, r); return -1; }
        int stream = d->packet->stream_index;
        AVCodecContext *ctx = stream == d->vi ? d->video : stream == d->ai ? d->audio : NULL;
        if (ctx) { r = avcodec_send_packet(ctx, d->packet); d->active = stream; }
        av_packet_unref(d->packet);
        if (ctx && r < 0) { error_text(error, len, r); return -1; }
    }
}
void flux_decoder_data_free(void *p) { free(p); }
