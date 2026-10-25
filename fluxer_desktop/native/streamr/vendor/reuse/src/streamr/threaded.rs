use super::{frontend::Frontend, vox::Vox, Engine, Pipeline};
use crate::Denoiser;
use eyre::{ensure, Result};
use std::{
    path::Path,
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    thread::JoinHandle,
};

enum Input {
    Audio(Vec<f32>),
    Finish,
}
pub enum Event {
    Audio(Vec<f32>),
    Finished,
}
type Output = std::result::Result<Event, String>;
type Ready = std::result::Result<(), String>;

fn initialized<T>(value: Result<T>, ready: SyncSender<Ready>) -> Result<T> {
    let _ = ready.send(
        value
            .as_ref()
            .map(|_| ())
            .map_err(|error| format!("{error:#}")),
    );
    value
}

trait Stage {
    fn process(&mut self, input: &[f32]) -> Result<Vec<f32>>;
    fn flush(&mut self) -> Result<Vec<f32>>;
}
impl Stage for Frontend {
    fn process(&mut self, input: &[f32]) -> Result<Vec<f32>> {
        Frontend::process(self, input)
    }
    fn flush(&mut self) -> Result<Vec<f32>> {
        Frontend::flush(self)
    }
}
impl Stage for Vox {
    fn process(&mut self, input: &[f32]) -> Result<Vec<f32>> {
        Vox::process(self, input)
    }
    fn flush(&mut self) -> Result<Vec<f32>> {
        Vox::flush(self)
    }
}
impl Stage for Denoiser {
    fn process(&mut self, input: &[f32]) -> Result<Vec<f32>> {
        Denoiser::process(self, input)
    }
    fn flush(&mut self) -> Result<Vec<f32>> {
        Denoiser::flush(self)
    }
}

#[cfg(feature = "vulkan")]
struct GpuTail {
    vox: Vox,
    reuse: Denoiser,
}
#[cfg(feature = "vulkan")]
impl GpuTail {
    fn load(root: &Path, enrollment: &[f32], engine: Engine) -> Result<Self> {
        Ok(Self {
            vox: Vox::load(root, enrollment, engine)?,
            reuse: Pipeline::load_reuse(root, engine)?,
        })
    }
}
#[cfg(feature = "vulkan")]
impl Stage for GpuTail {
    fn process(&mut self, input: &[f32]) -> Result<Vec<f32>> {
        self.reuse.process(&self.vox.process(input)?)
    }
    fn flush(&mut self) -> Result<Vec<f32>> {
        let mut audio = self.reuse.process(&self.vox.flush()?)?;
        audio.extend(self.reuse.flush()?);
        Ok(audio)
    }
}

fn spawn(
    name: &str,
    output: SyncSender<Output>,
    work: impl FnOnce() -> Result<()> + Send + 'static,
) -> Result<JoinHandle<()>> {
    let name = name.to_owned();
    Ok(std::thread::Builder::new()
        .name(name.clone())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
            let error = match result {
                Ok(Ok(())) => return,
                Ok(Err(error)) => format!("{name}: {error:#}"),
                Err(_) => format!("{name} panicked"),
            };
            let _ = output.send(Err(error));
        })?)
}
fn trace_stage(start: Option<std::time::Instant>, stage: &str, operation: &str) {
    if let Some(start) = start {
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        if elapsed >= 25.0 {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            eprintln!("STREAMR_STAGE {now} {stage} {operation} {elapsed:.3}ms");
        }
    }
}

fn forward(
    mut stage: impl Stage,
    input: Receiver<Input>,
    output: SyncSender<Input>,
    name: &str,
    trace: bool,
) -> Result<()> {
    for message in input {
        let started = trace.then(std::time::Instant::now);
        let (audio, finished) = match message {
            Input::Audio(input) => (stage.process(&input)?, false),
            Input::Finish => (stage.flush()?, true),
        };
        trace_stage(started, name, "process");
        let started = trace.then(std::time::Instant::now);
        if output.send(Input::Audio(audio)).is_err() {
            return Ok(());
        }
        trace_stage(started, name, "queue");
        if finished {
            let _ = output.send(Input::Finish);
            break;
        }
    }
    Ok(())
}

fn finish(
    mut stage: impl Stage,
    input: Receiver<Input>,
    output: SyncSender<Output>,
    name: &str,
    trace: bool,
) -> Result<()> {
    for message in input {
        let started = trace.then(std::time::Instant::now);
        let (audio, finished) = match message {
            Input::Audio(input) => (stage.process(&input)?, false),
            Input::Finish => (stage.flush()?, true),
        };
        trace_stage(started, name, "process");
        let started = trace.then(std::time::Instant::now);
        if output.send(Ok(Event::Audio(audio))).is_err() {
            return Ok(());
        }
        trace_stage(started, name, "queue");
        if finished {
            let _ = output.send(Ok(Event::Finished));
            break;
        }
    }
    Ok(())
}

pub struct ThreadedPipeline {
    input: Option<SyncSender<Input>>,
    output: Option<Receiver<Output>>,
    workers: Vec<JoinHandle<()>>,
    finishing: bool,
    input_count: usize,
    output_count: usize,
}
impl ThreadedPipeline {
    pub fn load_warmed(root: &Path, enrollment: &[f32], engine: Engine) -> Result<Self> {
        Self::load_internal(root, enrollment, engine, true)
    }
    pub fn load(root: &Path, enrollment: &[f32], engine: Engine) -> Result<Self> {
        Self::load_internal(root, enrollment, engine, false)
    }
    fn load_internal(
        root: &Path,
        enrollment: &[f32],
        engine: Engine,
        warm_gpu: bool,
    ) -> Result<Self> {
        let trace = std::env::var_os("STREAMR_TRACE").is_some();
        let enrollment = Pipeline::enrollment(root, enrollment)?;
        let front_root = root.to_owned();
        let vox_root = root.to_owned();
        let reuse_root = root.to_owned();
        // Four live 20ms blocks absorb scheduling stalls within the playback reserve.
        let (input, front_rx) = mpsc::sync_channel(4);
        let (front_tx, vox_rx) = mpsc::sync_channel(2);
        let (output_tx, output) = mpsc::sync_channel(2);
        let mut result = Self {
            input: Some(input),
            output: Some(output),
            workers: Vec::new(),
            finishing: false,
            input_count: 0,
            output_count: 0,
        };
        let (ready_tx, ready_rx) = mpsc::sync_channel(3);
        let front_ready = ready_tx.clone();
        result
            .workers
            .push(spawn("streamr-resample", output_tx.clone(), move || {
                let frontend = initialized(Frontend::load(&front_root), front_ready)?;
                forward(frontend, front_rx, front_tx, "resample", trace)
            })?);
        match engine {
            #[cfg(feature = "vulkan")]
            Engine::Vulkan => {
                result
                    .workers
                    .push(spawn("streamr-gpu", output_tx.clone(), move || {
                        let loaded = (|| -> Result<_> {
                            if warm_gpu {
                                let mut warm = GpuTail::load(&vox_root, &enrollment, engine)?;
                                for _ in 0..50 {
                                    warm.process(&[0.0; 320])?;
                                }
                                warm.flush()?;
                            }
                            GpuTail::load(&vox_root, &enrollment, engine)
                        })();
                        let tail = initialized(loaded, ready_tx)?;
                        finish(tail, vox_rx, output_tx, "gpu", trace)
                    })?);
            }
            Engine::Cpu => {
                let (vox_tx, reuse_rx) = mpsc::sync_channel(2);
                let vox_ready = ready_tx.clone();
                result
                    .workers
                    .push(spawn("streamr-vox", output_tx.clone(), move || {
                        let vox =
                            initialized(Vox::load(&vox_root, &enrollment, engine), vox_ready)?;
                        forward(vox, vox_rx, vox_tx, "vox", trace)
                    })?);
                result
                    .workers
                    .push(spawn("streamr-reuse", output_tx.clone(), move || {
                        let reuse =
                            initialized(Pipeline::load_reuse(&reuse_root, engine), ready_tx)?;
                        finish(reuse, reuse_rx, output_tx, "reuse", trace)
                    })?);
            }
        }
        for _ in 0..result.workers.len() {
            ready_rx
                .recv()
                .map_err(|_| eyre::eyre!("pipeline worker failed to initialize"))?
                .map_err(|error| eyre::eyre!(error))?;
        }
        Ok(result)
    }
    pub fn try_push(&mut self, samples: &[f32]) -> Result<bool> {
        ensure!(!self.finishing, "pipeline already finishing");
        ensure!(samples.iter().all(|x| x.is_finite()), "nonfinite input");
        match self
            .input
            .as_ref()
            .unwrap()
            .try_send(Input::Audio(samples.to_vec()))
        {
            Ok(()) => {
                self.input_count += samples.len();
                Ok(true)
            }
            Err(TrySendError::Full(_)) => Ok(false),
            Err(TrySendError::Disconnected(_)) => eyre::bail!("pipeline input closed"),
        }
    }
    pub fn try_finish(&mut self) -> Result<bool> {
        ensure!(!self.finishing, "pipeline already finishing");
        match self.input.as_ref().unwrap().try_send(Input::Finish) {
            Ok(()) => {
                self.finishing = true;
                Ok(true)
            }
            Err(TrySendError::Full(_)) => Ok(false),
            Err(TrySendError::Disconnected(_)) => eyre::bail!("pipeline input closed"),
        }
    }
    pub fn try_output(&mut self) -> Result<Option<Event>> {
        match self.output.as_ref().unwrap().try_recv() {
            Ok(Ok(event)) => {
                match &event {
                    Event::Audio(audio) => {
                        ensure!(
                            audio.iter().all(|x| x.is_finite()),
                            "nonfinite pipeline output"
                        );
                        self.output_count += audio.len();
                    }
                    Event::Finished => ensure!(
                        self.output_count == self.input_count.div_ceil(3),
                        "pipeline sample count mismatch"
                    ),
                }
                Ok(Some(event))
            }
            Ok(Err(error)) => eyre::bail!(error),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => eyre::bail!("pipeline output closed"),
        }
    }
}
impl Drop for ThreadedPipeline {
    fn drop(&mut self) {
        self.output.take();
        self.input.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}
