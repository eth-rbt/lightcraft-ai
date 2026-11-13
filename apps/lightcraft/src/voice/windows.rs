//! All blocking networking and device ownership stays off the UI thread.

use super::audio::{MIC_SAMPLES, PCM_RATE, Playback, Resampler, pcm_sample};
use super::protocol::{self, TurnGate};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use lightcraft_ui_egui::assistant::{VoiceConnection, VoiceEvent, VoiceInput};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tungstenite::client::IntoClientRequest;
use tungstenite::http::HeaderValue;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

#[derive(Clone)]
struct Emitter {
    sender: SyncSender<VoiceEvent>,
    ctx: egui::Context,
}

impl Emitter {
    fn emit(&self, event: VoiceEvent) -> bool {
        let sent = self.sender.try_send(event).is_ok();
        self.ctx.request_repaint();
        sent
    }

    fn status(&self, text: &str) {
        self.emit(VoiceEvent::Status(text.into()));
    }
}

pub fn start(ctx: egui::Context) -> Result<VoiceConnection, String> {
    let key = super::config::api_key()?;
    let model = setting("OPENAI_REALTIME_MODEL", "gpt-realtime-2.1")?;
    let voice = setting("OPENAI_VOICE", "marin")?;
    let (commands, receiver) = mpsc::sync_channel(8);
    let (sender, events) = mpsc::sync_channel(32);
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = Arc::clone(&stop);
    let emitter = Emitter { sender, ctx };
    std::thread::Builder::new()
        .name("lightcraft-voice".into())
        .spawn(move || {
            emitter.status("Connecting voice…");
            let result = run(&key, &model, &voice, receiver, &stopped, &emitter);
            // run has dropped the devices and socket before announcing termination.
            if let Err(message) = result {
                emitter.emit(VoiceEvent::Error(message));
            } else {
                emitter.status("Voice stopped");
            }
            stopped.store(true, Ordering::Release);
        })
        .map_err(|_| "Could not start the voice worker.".to_string())?;
    Ok(VoiceConnection { commands, events, stop })
}

fn setting(name: &str, fallback: &str) -> Result<String, String> {
    let value = std::env::var(name).unwrap_or_else(|_| fallback.into());
    if value.is_empty() || value.len() > 128 || !value.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)) {
        return Err(format!("{name} contains an invalid model or voice name."));
    }
    Ok(value)
}

struct StopWatch {
    done: Arc<AtomicBool>,
}

impl StopWatch {
    fn new(stream: &TcpStream, stop: Arc<AtomicBool>) -> Result<Self, String> {
        let stream = stream.try_clone().map_err(|_| "Could not configure voice cancellation.")?;
        let done = Arc::new(AtomicBool::new(false));
        let finished = Arc::clone(&done);
        std::thread::Builder::new()
            .name("lightcraft-voice-stop".into())
            .spawn(move || {
                while !finished.load(Ordering::Acquire) {
                    if stop.load(Ordering::Acquire) {
                        let _ = stream.shutdown(Shutdown::Both);
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            })
            .map_err(|_| "Could not start voice cancellation worker.")?;
        Ok(Self { done })
    }
}

impl Drop for StopWatch {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
    }
}

fn connect(key: &str, model: &str, stop: &Arc<AtomicBool>) -> Result<(Socket, StopWatch), String> {
    // The OS resolver has no cancellation API. Its isolated helper holds no audio
    // device or credentials and cannot keep the session worker alive on Stop.
    let (resolved, addresses) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("lightcraft-voice-dns".into())
        .spawn(move || {
            let result = ("api.openai.com", 443).to_socket_addrs().map(|addresses| addresses.take(4).collect::<Vec<_>>());
            let _ = resolved.try_send(result);
        })
        .map_err(|_| "Could not start voice lookup.")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let addresses = loop {
        if stop.load(Ordering::Acquire) {
            return Err("Voice connection cancelled.".into());
        }
        if Instant::now() >= deadline {
            return Err("Voice service lookup timed out.".into());
        }
        match addresses.recv_timeout(Duration::from_millis(20)) {
            Ok(result) => break result.map_err(|_| "Could not resolve the voice service.")?,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err("Voice service lookup stopped.".into()),
        }
    };
    let mut connected = None;
    for address in addresses {
        if stop.load(Ordering::Acquire) {
            return Err("Voice connection cancelled.".into());
        }
        if let Ok(stream) = TcpStream::connect_timeout(&address, Duration::from_millis(500)) {
            connected = Some(stream);
            break;
        }
    }
    let stream = connected.ok_or("Could not connect to the voice service.")?;
    let stop_watch = StopWatch::new(&stream, Arc::clone(stop))?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).map_err(|_| "Could not configure voice networking.")?;
    stream.set_write_timeout(Some(Duration::from_secs(2))).map_err(|_| "Could not configure voice networking.")?;
    let mut request =
        format!("wss://api.openai.com/v1/realtime?model={model}").into_client_request().map_err(|_| "Invalid voice connection configuration.")?;
    let authorization = HeaderValue::from_str(&format!("Bearer {}", key.trim())).map_err(|_| "The API key is not a valid HTTP credential.")?;
    request.headers_mut().insert("Authorization", authorization);
    let config = tungstenite::protocol::WebSocketConfig::default()
        .write_buffer_size(0)
        .max_write_buffer_size(8 * 1024 * 1024)
        .max_message_size(Some(2 * 1024 * 1024))
        .max_frame_size(Some(2 * 1024 * 1024));
    // Match crates/fetch: explicit RustCrypto avoids both C crypto and rustls's
    // panicking process-global provider auto-selection. WebPKI still verifies certificates.
    let connector = tungstenite::Connector::Rustls(tls_config()?);
    let (mut socket, _) = tungstenite::client_tls_with_config(request, stream, Some(config), Some(connector))
        .map_err(|_| "Voice authentication or TLS connection failed. Check key, model access and billing.")?;
    if let Some(stream) = tcp(&mut socket) {
        stream.set_read_timeout(Some(Duration::from_millis(20))).map_err(|_| "Could not configure voice polling.")?;
        stream.set_write_timeout(Some(Duration::from_millis(300))).map_err(|_| "Could not configure voice polling.")?;
    }
    Ok((socket, stop_watch))
}

fn tls_config() -> Result<Arc<rustls::ClientConfig>, String> {
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls_rustcrypto::provider()))
        .with_safe_default_protocol_versions()
        .map_err(|_| "Could not configure voice TLS.")?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

fn tcp(socket: &mut Socket) -> Option<&TcpStream> {
    match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => Some(stream),
        MaybeTlsStream::Rustls(stream) => Some(stream.get_ref()),
        _ => None,
    }
}

fn send(socket: &mut Socket, event: Value) -> Result<(), String> {
    socket.send(Message::Text(event.to_string().into())).map_err(|_| "Voice connection could not send data. Restart voice to continue.".into())
}

struct Frame {
    data_url: String,
    revision: u64,
    context: String,
    received: Instant,
}

struct PendingTool {
    call_id: String,
    resume: bool,
    started: Instant,
}

struct Cancellation {
    event_id: String,
    response_generation: u64,
}

struct Conversation {
    frame: Option<Frame>,
    sequence: u64,
    sent_revision: Option<u64>,
    previous_screen: Option<String>,
    screen_serial: u64,
    gate: Option<TurnGate>,
    committed: bool,
    response_active: bool,
    response_generation: u64,
    cancel_serial: u64,
    cancellations: VecDeque<Cancellation>,
    discard_audio: bool,
    transcript: String,
    pending_tool: Option<PendingTool>,
    continuation: bool,
}

impl Conversation {
    fn new() -> Self {
        Self {
            frame: None,
            sequence: 0,
            sent_revision: None,
            previous_screen: None,
            screen_serial: 0,
            gate: None,
            committed: false,
            response_active: false,
            response_generation: 0,
            cancel_serial: 0,
            cancellations: VecDeque::new(),
            discard_audio: false,
            transcript: String::new(),
            pending_tool: None,
            continuation: false,
        }
    }

    fn cancellation(&mut self) -> Result<Value, String> {
        self.cancel_serial = self.cancel_serial.checked_add(1).ok_or("Voice session exceeded its cancellation limit. Reconnect voice.")?;
        let event_id = format!("lc_cancel_{}", self.cancel_serial);
        // Successful cancels report response.done rather than an acknowledgement of this
        // event ID. Keep only a small recent window for errors already in transit.
        if self.cancellations.len() >= 16 {
            self.cancellations.pop_front();
        }
        self.cancellations.push_back(Cancellation { event_id: event_id.clone(), response_generation: self.response_generation });
        Ok(json!({"type":"response.cancel","event_id":event_id}))
    }

    fn server_error(&mut self, event: &Value) -> Result<(), String> {
        // Only this exact error, tied to an event we issued, is a benign race.
        // Never forward provider error messages, which may contain request details.
        if event.pointer("/error/code").and_then(Value::as_str) == Some("response_cancel_not_active")
            && let Some(event_id) = event.pointer("/error/event_id").and_then(Value::as_str)
            && let Some(index) = self.cancellations.iter().position(|cancel| cancel.event_id == event_id)
            && let Some(cancel) = self.cancellations.remove(index)
        {
            if cancel.response_generation == self.response_generation {
                // The server says this response is already terminal. Allow the pending
                // feedback turn through its normal visual/commit gate even if done was
                // not observed. An error for an earlier turn cannot unblock a newer one.
                self.response_active = false;
            }
            return Ok(());
        }
        Err("The voice service rejected a request. Check model/voice configuration, key access and billing.".into())
    }

    fn interrupt(&mut self, socket: &mut Socket, audio: &Audio, emitter: &Emitter, cancel: bool) -> Result<(), String> {
        if !emitter.emit(VoiceEvent::Interrupt) {
            return Err("Voice feedback queue is busy. Restart voice to continue.".into());
        }
        self.gate = None;
        self.continuation = false;
        self.discard_audio = true;
        if let Some(tool) = &mut self.pending_tool {
            tool.resume = false;
        }
        let cursors = audio.playback.lock().map_err(|_| "Voice playback state is unavailable.")?.interrupt(self.response_active);
        if cancel && self.response_active {
            let cancellation = self.cancellation()?;
            send(socket, cancellation)?;
        }
        for heard in cursors {
            send(
                socket,
                json!({"type":"conversation.item.truncate","item_id":heard.item,
                "content_index":heard.content_index,"audio_end_ms":heard.frames.saturating_mul(1000) / u64::from(audio.output_rate)}),
            )?;
        }
        Ok(())
    }

    fn input(&mut self, input: VoiceInput, socket: &mut Socket, audio: &Audio, emitter: &Emitter) -> Result<(), String> {
        match input {
            VoiceInput::Frame { data_url, revision, context } => {
                if data_url.len() <= protocol::MAX_FRAME_BYTES
                    && context.len() <= protocol::MAX_CONTEXT_BYTES
                    && (data_url.starts_with("data:image/png;base64,") || data_url.starts_with("data:image/jpeg;base64,"))
                {
                    self.sequence = self.sequence.saturating_add(1);
                    self.frame = Some(Frame { data_url, revision, context, received: Instant::now() });
                }
            }
            VoiceInput::Text(text) => {
                if text.trim().is_empty() {
                    return Ok(());
                }
                if text.len() > protocol::MAX_TEXT_BYTES {
                    return Err("Voice text feedback is too long.".into());
                }
                self.interrupt(socket, audio, emitter, true)?;
                send(socket, protocol::text(&text))?;
                self.gate = Some(TurnGate::new(self.sequence, false));
                emitter.status("Considering feedback…");
            }
            VoiceInput::ToolResult { call_id, result } => {
                if self.pending_tool.as_ref().is_some_and(|pending| pending.call_id == call_id) {
                    let pending = self.pending_tool.take();
                    let output = if result.to_string().len() <= protocol::MAX_TOOL_BYTES {
                        result
                    } else {
                        json!({"ok":false,"error":"Tool result exceeded size limit."})
                    };
                    send(socket, protocol::result(&call_id, &output))?;
                    if pending.is_some_and(|pending| pending.resume) {
                        self.continuation = true;
                    }
                }
            }
        }
        Ok(())
    }

    fn maybe_respond(&mut self, socket: &mut Socket, emitter: &Emitter) -> Result<(), String> {
        if self.response_active || self.pending_tool.is_some() {
            return Ok(());
        }
        if self.continuation && self.gate.is_none() {
            // The latest post-animation screenshot must accompany the continuation too.
            self.gate = Some(TurnGate::new(self.sequence, false));
            self.continuation = false;
        }
        let has_frame = self.frame.as_ref().is_some_and(|frame| frame.received.elapsed() < Duration::from_secs(5));
        if !self.gate.as_ref().is_some_and(|gate| gate.ready(self.sequence, has_frame, self.committed, Instant::now())) {
            return Ok(());
        }
        let Some(frame) = self.frame.as_ref() else {
            return Ok(());
        };
        if let Some(previous) = self.previous_screen.take() {
            send(socket, json!({"type":"conversation.item.delete","item_id":previous}))?;
        }
        self.screen_serial = self.screen_serial.saturating_add(1);
        send(socket, protocol::screen(&frame.data_url, frame.revision, &frame.context, self.screen_serial))?;
        self.previous_screen = Some(format!("lc_screen_{}", self.screen_serial));
        self.sent_revision = Some(frame.revision);
        self.gate = None;
        self.discard_audio = false;
        self.transcript.clear();
        self.response_generation = self.response_generation.checked_add(1).ok_or("Voice session exceeded its turn limit. Reconnect voice.")?;
        send(socket, json!({"type":"response.create"}))?;
        self.response_active = true;
        emitter.status("Responding…");
        Ok(())
    }

    fn server(&mut self, event: Value, socket: &mut Socket, audio: &mut Audio, emitter: &Emitter) -> Result<(), String> {
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "session.updated" => emitter.status("Listening · microphone and editor view shared"),
            "input_audio_buffer.speech_started" => {
                self.committed = false;
                self.interrupt(socket, audio, emitter, false)?;
                emitter.status("Listening…");
            }
            "input_audio_buffer.speech_stopped" => {
                self.gate = Some(TurnGate::new(self.sequence, true));
                emitter.status("Updating editor view…");
            }
            "input_audio_buffer.committed" => self.committed = true,
            "response.created" => self.response_active = true,
            "response.done" => {
                self.response_active = false;
                if event.pointer("/response/status").and_then(Value::as_str) == Some("failed") {
                    return Err("The voice model could not complete its response. Check model access and billing.".into());
                }
                if self.pending_tool.is_none() && self.gate.is_none() && !self.continuation {
                    emitter.status("Listening…");
                }
            }
            "response.output_audio.delta" if !self.discard_audio => audio.append(&event)?,
            "response.output_audio_transcript.delta" if !self.discard_audio => {
                if let Some(delta) = event.get("delta").and_then(Value::as_str)
                    && self.transcript.len().saturating_add(delta.len()) <= protocol::MAX_TEXT_BYTES
                {
                    self.transcript.push_str(delta);
                    emitter.emit(VoiceEvent::Transcript(self.transcript.clone()));
                }
            }
            "response.function_call_arguments.done" => self.tool(&event, socket, emitter)?,
            "error" => self.server_error(&event)?,
            _ => {}
        }
        Ok(())
    }

    fn tool(&mut self, event: &Value, socket: &mut Socket, emitter: &Emitter) -> Result<(), String> {
        let call_id = event.get("call_id").and_then(Value::as_str).ok_or("Voice tool call is missing an ID.")?;
        if call_id.is_empty() || call_id.len() > 128 {
            return Err("Invalid voice tool call ID.".into());
        }
        let name = event.get("name").and_then(Value::as_str).unwrap_or("");
        let arguments = event.get("arguments").and_then(Value::as_str).unwrap_or("");
        let parsed = if self.discard_audio {
            Err("Request interrupted by new feedback.")
        } else if self.pending_tool.is_some() {
            Err("Wait for the current action to finish.")
        } else {
            protocol::parse_tool(name, arguments, self.sent_revision)
        };
        let event = match parsed {
            Ok(protocol::Tool::Annotate { revision, operations }) => VoiceEvent::Annotate { call_id: call_id.into(), revision, operations },
            Ok(protocol::Tool::Edit { revision, control, value }) => VoiceEvent::Edit { call_id: call_id.into(), revision, control, value },
            Err(error) => {
                send(socket, protocol::result(call_id, &json!({"ok":false,"error":error})))?;
                if !self.discard_audio && self.pending_tool.is_none() {
                    self.continuation = true;
                }
                return Ok(());
            }
        };
        if emitter.emit(event) {
            self.pending_tool = Some(PendingTool { call_id: call_id.into(), resume: true, started: Instant::now() });
            emitter.status("Applying requested change…");
        } else {
            send(socket, protocol::result(call_id, &json!({"ok":false,"error":"Editor is busy; no action was applied."})))?;
            self.continuation = true;
        }
        Ok(())
    }
}

fn run(key: &str, model: &str, voice: &str, commands: Receiver<VoiceInput>, stop: &Arc<AtomicBool>, emitter: &Emitter) -> Result<(), String> {
    let (mut socket, _stop_watch) = connect(key, model, stop)?;
    if stop.load(Ordering::Acquire) {
        return Ok(());
    }
    send(&mut socket, protocol::session(model, voice))?;
    let mut audio = Audio::open(Arc::clone(stop), emitter.clone())?;
    let mut conversation = Conversation::new();
    let result = session_loop(&mut socket, &mut audio, &commands, stop, emitter, &mut conversation);
    // Silence callbacks before the close handshake, including on network failure.
    stop.store(true, Ordering::Release);
    drop(audio);
    let _ = socket.close(None);
    if let Some(stream) = tcp(&mut socket) {
        let _ = stream.shutdown(Shutdown::Both);
    }
    result
}

fn session_loop(
    socket: &mut Socket,
    audio: &mut Audio,
    commands: &Receiver<VoiceInput>,
    stop: &AtomicBool,
    emitter: &Emitter,
    conversation: &mut Conversation,
) -> Result<(), String> {
    while !stop.load(Ordering::Acquire) {
        if audio.failed.load(Ordering::Acquire) {
            return Err("The microphone or speaker disconnected. Restart voice after checking audio devices.".into());
        }
        for _ in 0..8 {
            match commands.try_recv() {
                Ok(input) => conversation.input(input, socket, audio, emitter)?,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()),
            }
        }
        if conversation.pending_tool.as_ref().is_some_and(|tool| tool.started.elapsed() > Duration::from_secs(20)) {
            return Err("The editor did not finish the requested action. Restart voice to continue.".into());
        }
        // Limit each pass so microphone traffic cannot starve interrupts or incoming audio.
        for _ in 0..4 {
            match audio.microphone.try_recv() {
                Ok(bytes) => send(socket, json!({"type":"input_audio_buffer.append","audio":STANDARD.encode(bytes)}))?,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Err("Microphone capture stopped.".into()),
            }
        }
        conversation.maybe_respond(socket, emitter)?;
        match socket.read() {
            Ok(Message::Text(text)) => {
                let event: Value = serde_json::from_str(text.as_str()).map_err(|_| "Invalid event from voice service.")?;
                conversation.server(event, socket, audio, emitter)?;
            }
            Ok(Message::Close(_)) => return Err("The voice connection closed. Start voice again to reconnect.".into()),
            Ok(Message::Ping(_)) => {
                socket.flush().map_err(|_| "Voice connection lost.")?;
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(error)) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => return Err("Voice connection lost. Start voice again to reconnect.".into()),
        }
    }
    Ok(())
}

struct Audio {
    _input: cpal::Stream,
    _output: cpal::Stream,
    microphone: Receiver<Vec<u8>>,
    playback: Arc<Mutex<Playback>>,
    output_rate: u32,
    resampler: Resampler,
    resample_item: String,
    failed: Arc<AtomicBool>,
}

impl Audio {
    fn open(stop: Arc<AtomicBool>, emitter: Emitter) -> Result<Self, String> {
        let host = cpal::default_host();
        let input = host.default_input_device().ok_or("No default microphone is available.")?;
        let output = host.default_output_device().ok_or("No default speaker is available.")?;
        let input_config = input.default_input_config().map_err(|_| "Could not read microphone format.")?;
        let output_config = output.default_output_config().map_err(|_| "Could not read speaker format.")?;
        let output_rate = output_config.sample_rate().0;
        if !(8_000..=192_000).contains(&input_config.sample_rate().0)
            || !(8_000..=192_000).contains(&output_rate)
            || input_config.channels() == 0
            || output_config.channels() == 0
        {
            return Err("Unsupported audio device sample rate or channel count.".into());
        }
        let playback = Arc::new(Mutex::new(Playback::new(output_rate)));
        let failed = Arc::new(AtomicBool::new(false));
        let (sender, microphone) = mpsc::sync_channel(8);
        let capture = Capture { sender, stop: Arc::clone(&stop), failed: Arc::clone(&failed), emitter: emitter.clone() };
        let input_stream = match input_config.sample_format() {
            cpal::SampleFormat::F32 => input_stream::<f32>(&input, &input_config.into(), capture),
            cpal::SampleFormat::I16 => input_stream::<i16>(&input, &input_config.into(), capture),
            cpal::SampleFormat::U16 => input_stream::<u16>(&input, &input_config.into(), capture),
            _ => return Err("The microphone must support float32, signed16 or unsigned16 audio.".into()),
        }?;
        let render = Render { playback: Arc::clone(&playback), stop, failed: Arc::clone(&failed), emitter };
        let output_stream = match output_config.sample_format() {
            cpal::SampleFormat::F32 => output_stream::<f32>(&output, &output_config.into(), render),
            cpal::SampleFormat::I16 => output_stream::<i16>(&output, &output_config.into(), render),
            cpal::SampleFormat::U16 => output_stream::<u16>(&output, &output_config.into(), render),
            _ => return Err("The speaker must support float32, signed16 or unsigned16 audio.".into()),
        }?;
        output_stream.play().map_err(|_| "Could not start the speaker. Check Windows audio settings.")?;
        input_stream.play().map_err(|_| "Could not start the microphone. Check Windows microphone permission.")?;
        Ok(Self {
            _input: input_stream,
            _output: output_stream,
            microphone,
            playback,
            output_rate,
            resampler: Resampler::new(PCM_RATE, output_rate),
            resample_item: String::new(),
            failed,
        })
    }

    fn append(&mut self, event: &Value) -> Result<(), String> {
        let encoded = event.get("delta").and_then(Value::as_str).ok_or("Missing voice audio.")?;
        if encoded.len() > 1024 * 1024 {
            return Err("Voice audio chunk exceeded size limit.".into());
        }
        let bytes = STANDARD.decode(encoded).map_err(|_| "Invalid voice audio encoding.")?;
        if bytes.len() % 2 != 0 {
            return Err("Invalid voice PCM audio.".into());
        }
        let item = event.get("item_id").and_then(Value::as_str).ok_or("Missing voice audio item.")?;
        if item.len() > 128 {
            return Err("Invalid voice audio item ID.".into());
        }
        let index = event.get("content_index").and_then(Value::as_u64).unwrap_or(0);
        if self.resample_item != item {
            self.resampler = Resampler::new(PCM_RATE, self.output_rate);
            self.resample_item = item.into();
        }
        let mut samples = Vec::new();
        for pair in bytes.chunks_exact(2) {
            if let (Some(low), Some(high)) = (pair.first(), pair.get(1)) {
                self.resampler.push(f32::from(i16::from_le_bytes([*low, *high])) / 32768.0, |sample| samples.push(sample));
            }
        }
        self.playback.lock().map_err(|_| "Voice playback state is unavailable.")?.append(item.into(), index, samples)
    }
}

struct Capture {
    sender: SyncSender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    emitter: Emitter,
}

fn input_stream<T>(device: &cpal::Device, config: &cpal::StreamConfig, capture: Capture) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let channels = usize::from(config.channels).max(1);
    let mut resampler = Resampler::new(config.sample_rate.0, PCM_RATE);
    let mut bytes = Vec::with_capacity(MIC_SAMPLES * 2);
    device
        .build_input_stream::<T, _, _>(
            config,
            move |data, _| {
                if capture.stop.load(Ordering::Acquire) {
                    return;
                }
                for frame in data.chunks_exact(channels) {
                    let mono = frame.iter().map(|sample| sample.to_sample::<f32>()).sum::<f32>() / channels as f32;
                    resampler.push(mono, |sample| {
                        bytes.extend_from_slice(&pcm_sample(sample).to_le_bytes());
                        if bytes.len() >= MIC_SAMPLES * 2 {
                            let packet = std::mem::replace(&mut bytes, Vec::with_capacity(MIC_SAMPLES * 2));
                            // Never wait in an audio callback. Congestion drops capture packets.
                            let _ = capture.sender.try_send(packet);
                        }
                    });
                }
            },
            move |_| {
                capture.failed.store(true, Ordering::Release);
                capture.emitter.emit(VoiceEvent::Error("Microphone stream failed. Check Windows audio settings.".into()));
            },
            None,
        )
        .map_err(|_| "Could not open the microphone. Check Windows microphone permission.".into())
}

struct Render {
    playback: Arc<Mutex<Playback>>,
    stop: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    emitter: Emitter,
}

fn output_stream<T>(device: &cpal::Device, config: &cpal::StreamConfig, render: Render) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let channels = usize::from(config.channels).max(1);
    device
        .build_output_stream::<T, _, _>(
            config,
            move |data, _| {
                data.fill(T::from_sample(0.0));
                if render.stop.load(Ordering::Acquire) {
                    return;
                }
                // The callback emits silence rather than block behind a network operation.
                if let Ok(mut playback) = render.playback.try_lock() {
                    for frame in data.chunks_exact_mut(channels) {
                        frame.fill(T::from_sample(playback.next_sample()));
                    }
                }
            },
            move |_| {
                render.failed.store(true, Ordering::Release);
                render.emitter.emit(VoiceEvent::Error("Speaker stream failed. Check Windows audio settings.".into()));
            },
            None,
        )
        .map_err(|_| "Could not open the speaker. Check Windows audio settings.".into())
}

#[cfg(test)]
mod tests {
    use super::{Conversation, StopWatch, tls_config};
    use serde_json::{Value, json};
    use std::io::Read;
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    #[test]
    fn explicit_pure_rust_tls_configuration_needs_no_global_provider() {
        assert!(tls_config().is_ok());
    }

    fn inactive_error(cancel: &Value) -> Value {
        json!({"type":"error","error":{"code":"response_cancel_not_active","event_id":cancel.get("event_id")}})
    }

    #[test]
    fn matched_inactive_cancellation_unblocks_the_pending_feedback_turn() -> Result<(), String> {
        let mut conversation = Conversation::new();
        conversation.response_generation = 4;
        conversation.response_active = true;
        conversation.discard_audio = true;
        conversation.gate = Some(super::TurnGate::new(8, false));
        let cancel = conversation.cancellation()?;
        assert_eq!(cancel.get("type").and_then(Value::as_str), Some("response.cancel"));
        conversation.server_error(&inactive_error(&cancel))?;
        assert!(!conversation.response_active);
        assert!(conversation.discard_audio, "old response audio stays discarded");
        assert!(conversation.gate.is_some(), "fresh visual context is still required");
        assert!(conversation.server_error(&inactive_error(&cancel)).is_err(), "consume each match once");
        Ok(())
    }

    #[test]
    fn unmatched_inactive_errors_and_other_provider_errors_stay_fatal() -> Result<(), String> {
        let mut conversation = Conversation::new();
        conversation.response_active = true;
        let cancel = conversation.cancellation()?;
        for error in [
            json!({"type":"error","error":{"code":"response_cancel_not_active","event_id":"not_ours"}}),
            json!({"type":"error","error":{"code":"response_cancel_not_active"}}),
            json!({"type":"error","error":{"code":"invalid_api_key","event_id":cancel.get("event_id")}}),
            json!({"type":"error","error":{"code":"invalid_value","event_id":cancel.get("event_id")}}),
        ] {
            assert!(conversation.server_error(&error).is_err());
            assert!(conversation.response_active);
        }
        Ok(())
    }

    #[test]
    fn late_cancellation_error_cannot_unblock_a_newer_response() -> Result<(), String> {
        let mut conversation = Conversation::new();
        conversation.response_generation = 7;
        let cancel = conversation.cancellation()?;
        conversation.response_generation = 8;
        conversation.response_active = true;
        conversation.server_error(&inactive_error(&cancel))?;
        assert!(conversation.response_active);
        Ok(())
    }

    #[test]
    fn cancellation_tracking_is_bounded_and_event_ids_are_distinct() -> Result<(), String> {
        let mut conversation = Conversation::new();
        let oldest = conversation.cancellation()?;
        let mut latest = Value::Null;
        for _ in 0..20 {
            latest = conversation.cancellation()?;
        }
        assert_eq!(conversation.cancellations.len(), 16);
        assert_ne!(oldest.get("event_id"), latest.get("event_id"));
        assert!(conversation.server_error(&inactive_error(&oldest)).is_err());
        assert!(conversation.server_error(&inactive_error(&latest)).is_ok());
        Ok(())
    }

    #[test]
    fn stop_closes_a_blocked_socket_without_waiting_for_server() -> Result<(), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let client = TcpStream::connect(listener.local_addr()?)?;
        let (mut server, _) = listener.accept()?;
        server.set_read_timeout(Some(Duration::from_secs(1)))?;
        let stop = Arc::new(AtomicBool::new(false));
        let _watch = StopWatch::new(&client, Arc::clone(&stop))?;
        stop.store(true, Ordering::Release);
        let mut byte = [0u8; 1];
        assert_eq!(server.read(&mut byte)?, 0);
        Ok(())
    }
}
