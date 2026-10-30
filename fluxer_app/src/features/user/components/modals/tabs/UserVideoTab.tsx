// SPDX-License-Identifier: AGPL-3.0-or-later

import RuntimeConfig from '@app/features/app/state/RuntimeConfig';
import {CAMERA_DESCRIPTOR as CAMERA_2_DESCRIPTOR} from '@app/features/i18n/utils/CommonMessageDescriptors';
import {ComponentBus} from '@app/features/platform/utils/ComponentBus';
import {getPremiumProductName, shouldShowPremiumFeatures} from '@app/features/premium/utils/PremiumUtils';
import {remFromPx} from '@app/features/theme/layout/RemFromPx';
import {Button} from '@app/features/ui/button/Button';
import * as ModalCommands from '@app/features/ui/commands/ModalCommands';
import {modal} from '@app/features/ui/commands/ModalCommands';
import type {ComboboxOption} from '@app/features/ui/components/form/FormCombobox';
import {Switch} from '@app/features/ui/components/form/FormSwitch';
import {getAdvancedSettingsCategorySectionId} from '@app/features/user/components/modals/tabs/advanced_settings_tab/AdvancedSettingsCategories';
import {CompactComboboxRow} from '@app/features/user/components/modals/tabs/components/CompactComboboxRow';
import {useMediaPermission} from '@app/features/user/components/modals/tabs/hooks/useMediaPermission';
import styles from '@app/features/user/components/modals/tabs/UserVideoTab.module.css';
import {
	resolveUserVideoTabScreenShareState,
	SCREEN_SHARE_PRESET_DESCRIPTORS,
} from '@app/features/user/components/modals/tabs/UserVideoTabState';
import {
	CAMERA_QUALITY_DESCRIPTOR,
	AUTOMATIC_DESCRIPTOR as SCREEN_SHARE_BITRATE_AUTOMATIC_DESCRIPTOR,
	SCREEN_SHARE_BITRATE_DESCRIPTOR,
	SCREEN_SHARE_QUALITY_DESCRIPTOR,
} from '@app/features/user/utils/UserMessageDescriptors';
import * as VoiceSettingsCommands from '@app/features/voice/commands/VoiceSettingsCommands';
import {CameraPreviewModalStandalone} from '@app/features/voice/components/modals/CameraPreviewModal';
import {
	CAPTURE_DEVICES_USE_THE_GAMING_PRESET_DESCRIPTOR,
	type OfferedScreenShareResolution,
} from '@app/features/voice/components/StreamSettingsMenuContentStateMachine';
import ActiveScreenShareSource from '@app/features/voice/state/ActiveScreenShareSource';
import type VoiceSettings from '@app/features/voice/state/VoiceSettings';
import type {CameraResolution} from '@app/features/voice/state/VoiceSettings';
import {
	resolveScreenShareQualityPick,
	type ScreenShareQualityInput,
	type SupportedScreenShareFrameRate,
} from '@app/features/voice/utils/ScreenShareOptions';
import {buildSettingsDeviceOptions} from '@app/features/voice/utils/SettingsDeviceOptions';
import {hasHigherVideoQuality} from '@app/features/voice/utils/VideoQualityEntitlement';
import {resolveEffectiveDeviceId} from '@app/features/voice/utils/VoiceDeviceManager';
import type {I18n} from '@lingui/core';
import {msg, ph} from '@lingui/core/macro';
import {Trans, useLingui} from '@lingui/react/macro';
import {AtomIcon, GearIcon} from '@phosphor-icons/react';
import {observer} from 'mobx-react-lite';
import type React from 'react';
import {useCallback, useMemo} from 'react';

const FPS_DESCRIPTOR = msg({
	message: '{frameRate} FPS',
	comment: 'Frame rate option label in the video tab. FPS is a technical token and frameRate is a whole number.',
});

const LOW_480P_LABEL = '480p';
const MEDIUM_720P_LABEL = '720p';
const HIGH_1080P_LABEL = '1080p';
const PRODUCT_NEEDS_CAMERA_ACCESS_DESCRIPTOR = msg({
	message: '{productName} needs camera access to list your devices.',
	comment: 'Camera settings notice shown when camera permission has not been granted.',
});
const SCREEN_SHARE_RESOLUTION_LABELS: Record<Exclude<OfferedScreenShareResolution, 'source'>, string> = {
	low_480p: '480p',
	medium: '720p',
	high: '1080p',
	ultra: '1440p',
};
const SOURCE_DESCRIPTOR = msg({
	message: 'Source',
	comment: 'Short label in the video tab for the native source resolution option. Keep it concise.',
});
const SCREEN_SHARE_PRESET_OWNED_DESCRIPTOR = msg({
	message: 'Set by the {preset} preset. Picking a value here switches to Custom.',
	comment:
		'Video settings note shown when a streaming preset decides the screen share resolution and frame rate. preset is the Gaming or Screen share preset label, and Custom is the third preset label.',
});
const SCREEN_SHARE_TIER_LIMITED_DESCRIPTOR = msg({
	message:
		'Your saved {savedResolution} at {savedFrameRate} FPS needs {premiumProductName}, so shares use {resolution} at {frameRate} FPS.',
	comment:
		'Video settings note shown when the saved screen share quality is above what this account can send. Resolutions are short labels such as 1440p or Source and rates are whole numbers. FPS is a technical token.',
});

const MIRROR_CAMERA_DESCRIPTOR = msg({
	message: 'Mirror camera',
	comment: 'Switch label in the video tab for flipping the local camera preview horizontally.',
});

const SCREEN_SHARE_BITRATE_DESCRIPTION_DESCRIPTOR = msg({
	message:
		'With Automatic, the bitrate follows your resolution and frame rate. Higher values keep fast motion sharper and use more upload bandwidth.',
	comment:
		'Description under the screen share bitrate select in the video tab. Automatic is the label of the default option.',
});
const SCREEN_SHARE_BITRATE_MBPS_DESCRIPTOR = msg({
	message: '{bitrate} Mbps',
	comment:
		'Option label of the screen share bitrate select in the video tab. bitrate is a formatted number of megabits per second. Mbps is a technical unit token.',
});

const ADVANCED_VIDEO_SETTINGS_DESCRIPTOR = msg({
	message: 'Advanced video settings',
	comment: 'Subsection title in the video tab. Keep it concise.',
});
const OPEN_ADVANCED_MEDIA_SETTINGS_DESCRIPTOR = msg({
	message: 'Open advanced media settings',
	comment: 'Button label in the video tab that jumps to the advanced settings media section.',
});

function getScreenShareResolutionLabel(i18n: I18n, resolution: OfferedScreenShareResolution): string {
	return resolution === 'source' ? i18n._(SOURCE_DESCRIPTOR) : SCREEN_SHARE_RESOLUTION_LABELS[resolution];
}

interface VideoTabProps {
	voiceSettings: typeof VoiceSettings;
}

export const VideoTab: React.FC<VideoTabProps> = observer(({voiceSettings}) => {
	const {i18n} = useLingui();
	const {
		videoDeviceId,
		cameraResolution,
		mirrorCamera,
		screenshareResolution,
		videoFrameRate,
		streamingMode,
		screenShareContentHint,
	} = voiceSettings;
	const hasHigherQuality = hasHigherVideoQuality();
	const showPremium = shouldShowPremiumFeatures();
	const {devices, deviceState, status: permissionStatus, requestPermission} = useMediaPermission('video');
	const videoDeviceOptions = useMemo(
		() => buildSettingsDeviceOptions(deviceState, 'videoinput', i18n),
		[deviceState, i18n.locale],
	);
	const effectiveVideoDeviceId = resolveEffectiveDeviceId(videoDeviceId, devices) ?? 'default';
	const cameraResolutionOptions: ReadonlyArray<ComboboxOption<CameraResolution>> = [
		{value: 'low', label: LOW_480P_LABEL},
		{value: 'medium', label: MEDIUM_720P_LABEL},
		...(hasHigherQuality || showPremium
			? [
					{
						value: 'high' as const,
						label: HIGH_1080P_LABEL,
						isDisabled: !hasHigherQuality,
					},
				]
			: []),
	];
	const liveShareContext = ActiveScreenShareSource.getShareContext();
	const screenShareQuality: ScreenShareQualityInput = {
		mode: streamingMode,
		storedResolution: screenshareResolution,
		storedFrameRate: videoFrameRate,
		entitled: true,
		context: liveShareContext ?? 'display',
	};
	const screenShareState = resolveUserVideoTabScreenShareState({
		quality: screenShareQuality,
		hintSetting: screenShareContentHint,
		bitrate: voiceSettings.getScreenShareBitrateBudget(),
		selfHosted: !showPremium,
	});
	const screenshareResolutionOptions: ReadonlyArray<ComboboxOption<OfferedScreenShareResolution>> =
		screenShareState.resolutionOptions.map((option) => ({
			value: option.value,
			label: getScreenShareResolutionLabel(i18n, option.value),
			isDisabled: option.isDisabled,
		}));
	const frameRateOptions: ReadonlyArray<ComboboxOption<SupportedScreenShareFrameRate>> =
		screenShareState.frameRateOptions.map((value) => ({
			value,
			label: i18n._(FPS_DESCRIPTOR, {frameRate: value}),
		}));
	const bitrateOptions: ReadonlyArray<ComboboxOption<number | null>> = [
		{value: null, label: i18n._(SCREEN_SHARE_BITRATE_AUTOMATIC_DESCRIPTOR)},
		...screenShareState.bitrateOptions.map((value) => ({
			value,
			label: i18n._(SCREEN_SHARE_BITRATE_MBPS_DESCRIPTOR, {
				bitrate: i18n.number(value / 1_000_000, {maximumFractionDigits: 1}),
			}),
		})),
	];
	const handleCameraPreview = async () => {
		const granted = await requestPermission();
		if (granted) {
			ModalCommands.push(
				modal(() => (
					<CameraPreviewModalStandalone
						showEnableCameraButton={false}
						data-flx="user.video-tab.handle-camera-preview.camera-preview-modal-standalone"
					/>
				)),
			);
		}
	};
	const handleScreenshareResolutionChange = (value: OfferedScreenShareResolution) => {
		const patch = resolveScreenShareQualityPick(screenShareQuality, {axis: 'resolution', resolution: value});
		if (patch === null) return;
		VoiceSettingsCommands.update(patch);
	};
	const handleVideoFrameRateChange = (value: SupportedScreenShareFrameRate) => {
		const patch = resolveScreenShareQualityPick(screenShareQuality, {axis: 'frameRate', frameRate: value});
		if (patch === null) return;
		VoiceSettingsCommands.update(patch);
	};
	const handleOpenAdvancedVideoSettings = useCallback(() => {
		ComponentBus.dispatch('USER_SETTINGS_TAB_SELECT', {
			tab: 'advanced_settings',
			section: getAdvancedSettingsCategorySectionId('media'),
		});
	}, []);
	const contentClassName = styles.content;
	let deviceNotice: React.ReactNode = null;
	if (devices.length === 0 && permissionStatus !== 'loading') {
		const noticeClassName = styles.deviceNotice;
		const noticeTextClassName = styles.deviceNoticeText;
		const noticeTitle = (
			<div className={styles.deviceNoticeTitle} data-flx="user.video-tab.device-notice-title">
				<Trans>No camera found</Trans>
			</div>
		);
		const descriptionClassName = styles.deviceNoticeDescription;
		let noticeDescription: React.ReactNode;
		if (permissionStatus === 'denied') {
			noticeDescription = (
				<Trans comment="Shown in video settings on the macOS app when camera access is denied. {ph({PRODUCT_NAME: RuntimeConfig.productName})} is the app name. Keep 'System Settings', 'Privacy & Security' and the permission name as macOS shows them in this language.">
					Allow {ph({PRODUCT_NAME: RuntimeConfig.productName})} to access your camera in System Settings → Privacy &
					Security → Camera.
				</Trans>
			);
		} else if (permissionStatus === 'granted') {
			noticeDescription = <Trans>Connect a camera and try again.</Trans>;
		} else {
			noticeDescription = i18n._(PRODUCT_NEEDS_CAMERA_ACCESS_DESCRIPTOR, {productName: RuntimeConfig.productName});
		}
		deviceNotice = (
			<div className={noticeClassName} data-flx="user.video-tab.device-notice">
				<div className={noticeTextClassName} data-flx="user.video-tab.device-notice-text">
					{noticeTitle}
					<p className={descriptionClassName} data-flx="user.video-tab.device-notice-description">
						{noticeDescription}
					</p>
				</div>
				{permissionStatus !== 'granted' ? (
					<Button
						variant="secondary"
						small={true}
						onClick={() => {
							void requestPermission();
						}}
						data-flx="user.video-tab.button"
					>
						<Trans>Allow camera</Trans>
					</Button>
				) : null}
			</div>
		);
	}
	return (
		<div className={contentClassName} data-flx="user.video-tab.content">
			{deviceNotice}
			<div className={styles.controlGroup} data-flx="user.video-tab.camera-settings-group">
				<CompactComboboxRow
					label={i18n._(CAMERA_2_DESCRIPTOR)}
					value={effectiveVideoDeviceId}
					options={videoDeviceOptions}
					onChange={(value) => VoiceSettingsCommands.update({videoDeviceId: value})}
					controlWidth="wide"
					menuMinWidth={280}
					dataFlx="user.video-tab.select.update"
					data-flx="user.user-video-tab.video-tab.compact-combobox-row.update"
				/>
				<div className={styles.cameraControlGroup} data-flx="user.video-tab.camera-control-group">
					<Switch
						label={i18n._(MIRROR_CAMERA_DESCRIPTOR)}
						value={mirrorCamera}
						onChange={(value) => VoiceSettingsCommands.update({mirrorCamera: value})}
						data-flx="user.user-video-tab.video-tab.switch.update"
					/>
					<Button
						variant="primary"
						fitContent
						className={styles.actionButton}
						onClick={handleCameraPreview}
						data-flx="user.video-tab.action-button.camera-preview"
					>
						<Trans>Start camera test and configure effects</Trans>
					</Button>
				</div>
				<CompactComboboxRow<CameraResolution>
					label={i18n._(CAMERA_QUALITY_DESCRIPTOR)}
					options={cameraResolutionOptions}
					value={cameraResolution}
					onChange={(value) => VoiceSettingsCommands.update({cameraResolution: value})}
					isSearchable={false}
					controlWidth="small"
					dataFlx="user.video-tab.select.camera-resolution-update"
					data-flx="user.user-video-tab.video-tab.compact-combobox-row.update--2"
				/>
			</div>
			<div className={styles.controlGroup} data-flx="user.video-tab.screen-sharing-group">
				<CompactComboboxRow<OfferedScreenShareResolution>
					label={i18n._(SCREEN_SHARE_QUALITY_DESCRIPTOR)}
					options={screenshareResolutionOptions}
					value={screenShareState.resolution}
					onChange={handleScreenshareResolutionChange}
					isSearchable={false}
					controlWidth="small"
					dataFlx="user.video-tab.select.screenshare-resolution-change"
					data-flx="user.user-video-tab.video-tab.compact-combobox-row.screenshare-resolution-change"
				/>
				<CompactComboboxRow<SupportedScreenShareFrameRate>
					label={<Trans>Frame rate</Trans>}
					options={frameRateOptions}
					value={screenShareState.frameRate}
					onChange={handleVideoFrameRateChange}
					isSearchable={false}
					controlWidth="small"
					dataFlx="user.video-tab.select.frame-rate-change"
					data-flx="user.user-video-tab.video-tab.compact-combobox-row.video-frame-rate-change"
				/>
				{screenShareState.preset !== null && (
					<div className={styles.frameRateNote} data-flx="user.video-tab.preset-note">
						{i18n._(SCREEN_SHARE_PRESET_OWNED_DESCRIPTOR, {
							preset: i18n._(SCREEN_SHARE_PRESET_DESCRIPTORS[screenShareState.preset]),
						})}
					</div>
				)}
				{screenShareState.presetOverriddenByContext && (
					<div className={styles.frameRateNote} data-flx="user.video-tab.preset-context-note">
						{i18n._(CAPTURE_DEVICES_USE_THE_GAMING_PRESET_DESCRIPTOR)}
					</div>
				)}
				{screenShareState.saved !== null && (
					<div className={styles.frameRateNote} data-flx="user.video-tab.tier-limit-note">
						<AtomIcon
							weight="fill"
							size={remFromPx(14)}
							className={styles.frameRateIcon}
							data-flx="user.video-tab.tier-limit-icon"
						/>
						{i18n._(SCREEN_SHARE_TIER_LIMITED_DESCRIPTOR, {
							frameRate: screenShareState.frameRate,
							premiumProductName: getPremiumProductName(),
							resolution: getScreenShareResolutionLabel(i18n, screenShareState.resolution),
							savedFrameRate: screenShareState.saved.frameRate,
							savedResolution: getScreenShareResolutionLabel(i18n, screenShareState.saved.resolution),
						})}
					</div>
				)}
				{hasHigherQuality && (
					<CompactComboboxRow<number | null>
						label={i18n._(SCREEN_SHARE_BITRATE_DESCRIPTOR)}
						description={i18n._(SCREEN_SHARE_BITRATE_DESCRIPTION_DESCRIPTOR)}
						options={bitrateOptions}
						value={screenShareState.bitrate}
						onChange={(value) => VoiceSettingsCommands.update({screenShareBitrate: value})}
						isSearchable={false}
						controlWidth="small"
						dataFlx="user.video-tab.select.screen-share-bitrate-change"
						data-flx="user.user-video-tab.video-tab.compact-combobox-row.screen-share-bitrate-change"
					/>
				)}
			</div>
			<div className={styles.actionRow} data-flx="user.video-tab.advanced-video-settings-section">
				<span className={styles.actionLabel} data-flx="user.video-tab.advanced-video-settings-label">
					{i18n._(ADVANCED_VIDEO_SETTINGS_DESCRIPTOR)}
				</span>
				<Button
					variant="secondary"
					fitContent
					leftIcon={
						<GearIcon size={remFromPx(16)} weight="bold" data-flx="user.video-tab.advanced-settings.gear-icon" />
					}
					onClick={handleOpenAdvancedVideoSettings}
					data-flx="user.video-tab.button.open-advanced-video-settings"
				>
					{i18n._(OPEN_ADVANCED_MEDIA_SETTINGS_DESCRIPTOR)}
				</Button>
			</div>
		</div>
	);
});
