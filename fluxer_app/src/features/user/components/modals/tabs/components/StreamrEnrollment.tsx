// SPDX-License-Identifier: AGPL-3.0-or-later

import {Button} from '@app/features/ui/button/Button';
import {getElectronAPI} from '@app/features/ui/utils/NativeUtils';
import styles from '@app/features/user/components/modals/tabs/components/StreamrEnrollment.module.css';
import VoiceSettings from '@app/features/voice/state/VoiceSettings';
import {acquireDeepFilterNode} from '@app/features/voice/utils/noise_suppression/DeepFilter';
import {selectEnrollmentSample} from '@app/features/voice/utils/noise_suppression/EnrollmentSample';
import NoiseSuppressionAvailability from '@app/features/voice/utils/noise_suppression/NoiseSuppressionAvailability';
import {setNoiseSuppressionChoice} from '@app/features/voice/utils/noise_suppression/NoiseSuppressionChoices';
import {observer} from 'mobx-react-lite';
import {useEffect, useRef, useState} from 'react';

export const StreamrEnrollment = observer(function StreamrEnrollment() {
	const api = getElectronAPI()?.streamr;
	const [phase, setPhase] = useState<'checking' | 'idle' | 'preparing' | 'recording' | 'saving'>('checking');
	const [error, setError] = useState<string | null>(null);
	const [available, setAvailable] = useState(false);
	const [elapsed, setElapsed] = useState(0);
	const operation = useRef<AbortController | null>(null);
	const mounted = useRef(false);
	const enrolled = NoiseSuppressionAvailability.streamrEnrolled === true;
	const busy = phase !== 'idle';
	useEffect(() => {
		mounted.current = true;
		let current = true;
		void api
			?.status()
			.then((status) => {
				if (!current) return;
				setAvailable(status.modelsAvailable);
				NoiseSuppressionAvailability.setStreamrEnrollment(status.enrolled);
				if (!status.modelsAvailable) setError('Streamr is unavailable in this installation.');
				setPhase('idle');
			})
			.catch(() => {
				if (!current) return;
				setError('Could not check your voice sample. Try reopening Voice Settings.');
				setPhase('idle');
			});
		return () => {
			current = false;
			mounted.current = false;
			operation.current?.abort();
		};
	}, [api]);
	const record = async () => {
		if (!api || operation.current) return;
		const controller = new AbortController();
		operation.current = controller;
		const {signal} = controller;
		setError(null);
		setElapsed(0);
		setPhase('preparing');
		let stream: MediaStream | undefined;
		let context: AudioContext | undefined;
		let dfn: Awaited<ReturnType<typeof acquireDeepFilterNode>> | undefined;
		let recorder: MediaRecorder | undefined;
		let timer: ReturnType<typeof setInterval> | undefined;
		const stop = () => {
			if (recorder?.state === 'recording') recorder.stop();
			stream?.getTracks().forEach((track) => track.stop());
		};
		signal.addEventListener('abort', stop, {once: true});
		try {
			stream = await navigator.mediaDevices.getUserMedia({
				audio: {
					deviceId: VoiceSettings.inputDeviceId === 'default' ? undefined : {exact: VoiceSettings.inputDeviceId},
					channelCount: 1,
					echoCancellation: false,
					noiseSuppression: false,
					autoGainControl: false,
				},
			});
			signal.throwIfAborted();
			context = new AudioContext({sampleRate: 48000});
			await context.resume();
			dfn = await acquireDeepFilterNode(context, signal, () =>
				controller.abort(new Error('Microphone processing stopped. Try again.')),
			);
			const destination = context.createMediaStreamDestination();
			context.createMediaStreamSource(stream).connect(dfn.node);
			dfn.node.connect(destination);
			await dfn.primed(signal);
			signal.throwIfAborted();
			recorder = new MediaRecorder(destination.stream);
			const chunks: Array<Blob> = [];
			const stopped = new Promise<Blob>((resolve, reject) => {
				recorder!.ondataavailable = (event) => {
					if (event.data.size) chunks.push(event.data);
				};
				recorder!.onstop = () => resolve(new Blob(chunks));
				recorder!.onerror = () => reject(new Error('Recording failed. Check your microphone and try again.'));
			});
			recorder.start();
			setPhase('recording');
			const started = performance.now();
			timer = setInterval(() => {
				const seconds = Math.min(10, (performance.now() - started) / 1000);
				if (mounted.current) setElapsed(seconds);
				if (seconds >= 10) stop();
			}, 100);
			const blob = await stopped;
			clearInterval(timer);
			signal.throwIfAborted();
			setPhase('saving');
			const decoded = await context.decodeAudioData(await blob.arrayBuffer());
			const channels = Array.from({length: decoded.numberOfChannels}, (_, i) => decoded.getChannelData(i));
			const sample = selectEnrollmentSample(channels, decoded.sampleRate);
			signal.throwIfAborted();
			await api.saveSample(sample);
			NoiseSuppressionAvailability.setStreamrEnrollment(true, true);
			if (mounted.current) setNoiseSuppressionChoice('streamr');
		} catch (cause) {
			if (mounted.current)
				setError(cause instanceof Error ? cause.message : 'Could not save your voice sample. Try again.');
		} finally {
			clearInterval(timer);
			signal.removeEventListener('abort', stop);
			stop();
			dfn?.node.disconnect();
			dfn?.release();
			await context?.close();
			operation.current = null;
			if (mounted.current) setPhase('idle');
		}
	};
	const clear = async () => {
		if (!api || busy) return;
		setPhase('saving');
		setError(null);
		try {
			await api.clearSample();
			NoiseSuppressionAvailability.setStreamrEnrollment(false, true);
		} catch {
			if (mounted.current) setError('Could not delete your voice sample. Try again.');
		} finally {
			if (mounted.current) setPhase('idle');
		}
	};
	let message = enrolled ? 'Voice sample ready.' : 'Record 10 seconds of your voice in a quiet room.';
	if (NoiseSuppressionAvailability.runningBackend === 'streamr') message = 'Streamr is active.';
	if (NoiseSuppressionAvailability.streamrStarting > 0) message = 'Starting Streamr…';
	if (phase === 'checking') message = 'Checking voice sample…';
	if (phase === 'preparing') message = 'Preparing microphone…';
	if (phase === 'recording') message = `Keep speaking — ${Math.ceil(10 - elapsed)} seconds left.`;
	if (phase === 'saving') message = 'Updating voice sample…';
	return (
		<div className={styles.enrollment}>
			<p className={styles.status} role={error ? 'alert' : 'status'}>
				{error ?? message}
			</p>
			{phase === 'recording' && (
				<progress className={styles.progress} max={10} value={elapsed} aria-label="Voice recording progress" />
			)}
			<div className={styles.actions}>
				<Button onClick={record} disabled={busy || !available} fitContainer>
					{enrolled ? 'Re-record voice' : 'Record voice'}
				</Button>
				<Button onClick={clear} disabled={busy || !enrolled} variant="secondary" fitContainer>
					Delete sample
				</Button>
			</div>
		</div>
	);
});
