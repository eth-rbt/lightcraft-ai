// Local chat backend: a browser page talks to Claude, and Claude's tool calls go to
// `lightcraft-cli mcp` running as a child process.
import Anthropic from "@anthropic-ai/sdk";
import { execFile, spawn, type ChildProcess } from "node:child_process";
import { access, mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import os from "node:os";
import path from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { McpClient, type McpToolResult } from "./mcp.ts";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const PORT = Number(process.env.PORT ?? 8787);
const MODEL = process.env.MODEL ?? "claude-opus-5-5";
const CLI = path.resolve(
  ROOT,
  process.env.LIGHTCRAFT_CLI ?? "../../target/release/lightcraft-cli",
);
const CLI_ARGS = (process.env.LIGHTCRAFT_MCP_ARGS ?? "mcp --demo --compact").split(/\s+/).filter(Boolean);
const UPLOADS = path.join(ROOT, "uploads");
const MAX_STEPS = 40; // model requests per user message
const MAX_BODY_BYTES = 80 * 1024 * 1024;
const VIEW_SIDE = 1024; // long edge of images shown to the model and the page

// Voice: speech is transcribed on this machine by whisper.cpp; nothing is sent to a speech service.
const WHISPER_CLI = process.env.WHISPER_CLI ?? "whisper-cli";
const WHISPER_MODEL = path.resolve(ROOT, process.env.WHISPER_MODEL ?? "models/ggml-small.en.bin");
/** ffmpeg input options for the microphone (macOS: the system's default input device). */
const VOICE_INPUT = (process.env.VOICE_INPUT ?? "-f avfoundation -i :default").split(/\s+/).filter(Boolean);
const BYTES_PER_SECOND = 32_000; // 16 kHz mono 16-bit, which is what Whisper reads
const MAX_UTTERANCE_BYTES = 5 * 60 * BYTES_PER_SECOND;
const HOLD_MS = 250; // the talk key held this long is talking; let go sooner, it is a tap
const PRE_ROLL_MS = 150; // audio kept from just before the key went down
const READY = "Hold right Option to talk";
/** Words Whisper should expect, so "dehaze" is not heard as "the haze". */
const WHISPER_PROMPT =
  "LightCraft photo editing: exposure, contrast, highlights, shadows, whites, blacks, temperature, tint, " +
  "vibrance, saturation, clarity, texture, dehaze, vignette, grain, sharpening, crop, mask, preset, undo.";
/** Added to a spoken message. It rides in the message, not the system prompt, so the prompt cache holds. */
const VOICE_NOTE =
  "[The message above was spoken and transcribed on the user's machine, so a word may be misheard: take the " +
  "most likely photo-editing meaning, or ask if it is unclear. Your reply to it is read aloud. For this turn, " +
  "in place of the written action log, say in one short sentence what you are about to do, and after the tools " +
  "finish say in one or two short sentences what you changed. No lists and no long runs of numbers: the page's " +
  "Action log already shows every tool call.]";

type ImageMedia = "image/jpeg" | "image/png" | "image/gif" | "image/webp";
const IMAGE_MEDIA: readonly string[] = ["image/jpeg", "image/png", "image/gif", "image/webp"];

type PageEvent =
  | { type: "text"; delta: string }
  | { type: "tool_call"; id: string; name: string; input: unknown }
  | { type: "tool_result"; id: string; isError: boolean; text: string; images: Image[] }
  | { type: "preview"; image: Image }
  | { type: "notice"; text: string }
  | { type: "error"; message: string }
  | { type: "done" };

interface Image {
  mimeType: string;
  data: string;
}

interface ChatRequest {
  text?: string;
  image?: { name: string; data: string };
  /** The text was spoken; the reply will be read aloud. */
  voice?: boolean;
}

/** One message to every open page: voice turns start on the server, so no page asked for them. */
type Broadcast =
  | PageEvent
  | { type: "user"; text: string }
  | { type: "voice"; state: string; agentMode: boolean };

let mcp: McpClient;
let tools: Anthropic.Tool[] = [];
let system = "";
let messages: Anthropic.MessageParam[] = [];
let busy = false;
let current: AbortController | null = null; // the reply in progress, typed or spoken

async function startLightcraft(): Promise<void> {
  mcp = await McpClient.start(CLI, CLI_ARGS);
  tools = (await mcp.listTools()).map((tool) => ({
    name: tool.name,
    description: tool.description ?? "",
    input_schema: tool.inputSchema,
  }));
  const prompt = (await readFile(path.join(ROOT, "system-prompt.txt"), "utf8")).trim();
  system = mcp.instructions ? `${prompt}\n\nNotes from the LightCraft server:\n${mcp.instructions}` : prompt;
}

function imagesOf(result: McpToolResult): Image[] {
  return result.content.flatMap((part) =>
    part.type === "image" && part.data && part.mimeType ? [{ mimeType: part.mimeType, data: part.data }] : [],
  );
}

function textOf(result: McpToolResult): string {
  return result.content.flatMap((part) => (part.type === "text" && part.text ? [part.text] : [])).join("\n");
}

function imageBlock(image: Image): Anthropic.ImageBlockParam | null {
  if (!IMAGE_MEDIA.includes(image.mimeType)) return null;
  return {
    type: "image",
    source: { type: "base64", media_type: image.mimeType as ImageMedia, data: image.data },
  };
}

/** What the model gets back for a tool call: the text, and any image so it can look at it. */
function toolResultContent(result: McpToolResult): Anthropic.ToolResultBlockParam["content"] {
  const blocks: (Anthropic.TextBlockParam | Anthropic.ImageBlockParam)[] = [];
  for (const image of imagesOf(result)) {
    const block = imageBlock(image);
    if (block) blocks.push(block);
  }
  const text = textOf(result);
  if (text) blocks.push({ type: "text", text });
  return blocks.length > 0 ? blocks : "(no output)";
}

async function currentPreview(): Promise<Image | null> {
  try {
    const result = await mcp.callTool("render_preview", { max_side: VIEW_SIDE, format: "jpeg" });
    return result.isError ? null : (imagesOf(result)[0] ?? null);
  } catch {
    return null;
  }
}

/** Saves an attached picture, imports it into LightCraft and returns what to tell the model. */
async function importAttachment(
  image: NonNullable<ChatRequest["image"]>,
): Promise<Anthropic.ContentBlockParam[]> {
  await mkdir(UPLOADS, { recursive: true });
  const safeName = path.basename(image.name).replace(/[^A-Za-z0-9._-]/g, "_") || "photo";
  const file = path.join(UPLOADS, `${Date.now()}-${safeName}`);
  await writeFile(file, Buffer.from(image.data, "base64"));

  const imported = await mcp.callTool("import", { paths: [file] });
  const note = `[The user attached "${image.name}". LightCraft import result: ${textOf(imported) || "(none)"}]`;
  if (imported.isError) return [{ type: "text", text: note }];

  // LightCraft renders it, so raw files and very large photos reach the model as a small JPEG.
  const view = await currentPreview();
  const block = view && imageBlock(view);
  return block
    ? [block, { type: "text", text: `${note}\n[The image above is that photo as LightCraft renders it now.]` }]
    : [{ type: "text", text: note }];
}

function describeError(error: unknown): string {
  if (error instanceof Anthropic.AuthenticationError) {
    return "The Claude API rejected the credentials. Put ANTHROPIC_API_KEY in tools/lightcraft-chat/.env and restart.";
  }
  if (error instanceof Anthropic.RateLimitError) return "Rate limited by the Claude API. Try again shortly.";
  if (error instanceof Anthropic.APIError) return `Claude API error ${error.status ?? ""}: ${error.message}`;
  if (error instanceof Anthropic.AnthropicError) {
    return `${error.message} (is ANTHROPIC_API_KEY set in tools/lightcraft-chat/.env?)`;
  }
  return error instanceof Error ? error.message : String(error);
}

async function runTurn(request: ChatRequest, send: (event: PageEvent) => void, signal: AbortSignal): Promise<void> {
  const anthropic = new Anthropic();
  const content: Anthropic.ContentBlockParam[] = [];
  if (request.image) {
    content.push(...(await importAttachment(request.image)));
    const preview = await currentPreview();
    if (preview) send({ type: "preview", image: preview });
  }
  const text = request.text?.trim();
  if (text) content.push({ type: "text", text });
  if (text && request.voice) content.push({ type: "text", text: VOICE_NOTE });
  if (content.length === 0) throw new Error("Nothing to send.");
  const before = messages.length;
  messages.push({ role: "user", content });
  try {
    await runSteps(anthropic, send, signal);
  } catch (error) {
    // Nothing came back at all (no key, network down): forget the message so a retry isn't doubled.
    if (messages.length === before + 1) messages.length = before;
    throw error;
  }
}

async function runSteps(anthropic: Anthropic, send: (event: PageEvent) => void, signal: AbortSignal): Promise<void> {
  for (let step = 0; step < MAX_STEPS; step++) {
    if (signal.aborted) return;
    const stream = anthropic.messages.stream(
      {
        model: MODEL,
        max_tokens: 64000,
        system,
        tools,
        messages,
        cache_control: { type: "ephemeral" },
      },
      { signal },
    );
    stream.on("text", (delta) => send({ type: "text", delta }));
    const message = await stream.finalMessage();

    if (message.stop_reason === "refusal") {
      send({ type: "notice", text: "The model declined this request." });
      return;
    }
    const calls = message.content.filter((block): block is Anthropic.ToolUseBlock => block.type === "tool_use");
    if (message.stop_reason === "max_tokens" && calls.length > 0) {
      throw new Error("The reply was cut off in the middle of a tool call, so nothing was run for it.");
    }
    // Thinking blocks go back unchanged with the rest of the content.
    messages.push({ role: "assistant", content: message.content });
    if (message.stop_reason === "pause_turn") continue;
    if (calls.length === 0) return;

    // One LightCraft session, so calls run in the order the model gave them.
    const results: Anthropic.ToolResultBlockParam[] = [];
    for (const call of calls) {
      send({ type: "tool_call", id: call.id, name: call.name, input: call.input });
      let result: McpToolResult;
      try {
        result = await mcp.callTool(call.name, call.input);
      } catch (error) {
        result = { content: [{ type: "text", text: describeError(error) }], isError: true };
      }
      send({
        type: "tool_result",
        id: call.id,
        isError: result.isError === true,
        text: textOf(result),
        images: imagesOf(result),
      });
      results.push({
        type: "tool_result",
        tool_use_id: call.id,
        content: toolResultContent(result),
        is_error: result.isError === true,
      });
    }
    messages.push({ role: "user", content: results });

    const preview = await currentPreview();
    if (preview) send({ type: "preview", image: preview });
  }
  send({ type: "notice", text: `Stopped after ${MAX_STEPS} steps. Say "continue" to carry on.` });
}

// The talk key: a small macOS helper reports the right Option key going down and up in any app.
// A tap turns agent mode on or off (LightCraft's toolbar, and the microphone); holding it is talking.
const TALK_KEY_SOURCE = path.join(ROOT, "tools", "talk-key.swift");
const TALK_KEY_BIN = path.join(ROOT, "bin", "talk-key");
const TALK_KEY_HELPER = process.env.TALK_KEY_HELPER; // another program printing the same lines; skips the build
const pages: ServerResponse[] = []; // open /api/events streams
let talkKey: ChildProcess | null = null;

function broadcast(event: Broadcast): void {
  for (const page of pages) page.write(`data: ${JSON.stringify(event)}\n\n`);
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** The microphone, through ffmpeg, as 16 kHz mono 16-bit samples. Open only while agent mode is on. */
class Microphone {
  private child: ChildProcess | null = null;
  private chunks: Buffer[] = [];
  private bytes = 0;
  private odd: Buffer | null = null; // half a sample left over from the last read
  private recording = false;

  open(): void {
    if (this.child) return;
    const args = ["-hide_banner", "-loglevel", "error", ...VOICE_INPUT, "-ac", "1", "-ar", "16000", "-f", "s16le", "-"];
    const child = spawn("ffmpeg", args, { stdio: ["ignore", "pipe", "pipe"] });
    this.child = child;
    let complaint = "";
    child.stderr?.on("data", (data: Buffer) => (complaint = (complaint + data.toString()).slice(-2000)));
    child.stdout?.on("data", (data: Buffer) => this.take(data));
    child.on("error", (error) => {
      const missing = (error as NodeJS.ErrnoException).code === "ENOENT";
      console.error(missing ? "No microphone: ffmpeg was not found. Install it with `brew install ffmpeg`." : `Microphone: ${error.message}`);
    });
    child.on("exit", (code) => {
      if (this.child !== child) return; // closed on purpose
      this.child = null;
      if (code) {
        console.error(
          `The microphone stopped (${complaint.trim().split("\n").at(-1) ?? `ffmpeg exit ${code}`}). ` +
            "Allow the app you ran `npm start` from under System Settings > Privacy & Security > Microphone.",
        );
      }
    });
  }

  close(): void {
    const child = this.child;
    this.child = null;
    child?.kill();
    this.chunks = [];
    this.bytes = 0;
    this.odd = null;
    this.recording = false;
  }

  private take(data: Buffer): void {
    let samples = this.odd ? Buffer.concat([this.odd, data]) : data;
    this.odd = samples.length % 2 ? samples.subarray(samples.length - 1) : null;
    if (this.odd) samples = samples.subarray(0, samples.length - 1);
    if (this.recording && this.bytes >= MAX_UTTERANCE_BYTES) return;
    this.chunks.push(samples);
    this.bytes += samples.length;
    if (!this.recording) this.keep(2 * BYTES_PER_SECOND);
  }

  /** Forget all but about the last `limit` bytes (whole reads, so samples stay aligned). */
  private keep(limit: number): void {
    while (this.chunks.length > 1 && this.bytes - (this.chunks[0]?.length ?? 0) >= limit) {
      this.bytes -= this.chunks.shift()?.length ?? 0;
    }
  }

  /** Start an utterance, keeping `preRollMs` of what was heard just before. */
  begin(preRollMs: number): void {
    this.open();
    this.keep((preRollMs / 1000) * BYTES_PER_SECOND);
    this.recording = true;
  }

  /** The utterance as a WAV file. */
  end(): Buffer {
    const pcm = Buffer.concat(this.chunks);
    this.chunks = [];
    this.bytes = 0;
    this.recording = false;
    const header = Buffer.alloc(44);
    header.write("RIFF", 0);
    header.writeUInt32LE(36 + pcm.length, 4);
    header.write("WAVEfmt ", 8);
    header.writeUInt32LE(16, 16);
    header.writeUInt16LE(1, 20); // PCM
    header.writeUInt16LE(1, 22); // mono
    header.writeUInt32LE(16_000, 24);
    header.writeUInt32LE(BYTES_PER_SECOND, 28);
    header.writeUInt16LE(2, 32);
    header.writeUInt16LE(16, 34);
    header.write("data", 36);
    header.writeUInt32LE(pcm.length, 40);
    return Buffer.concat([header, pcm]);
  }
}

/** Reads the reply aloud with the system voice (`say`), a sentence at a time as it streams. */
class Speaker {
  private queue: string[] = [];
  private unspoken = "";
  private child: ChildProcess | null = null;

  /** Queue the finished sentences of streamed text; `all` also queues the unfinished rest. */
  feed(delta: string, all = false): void {
    this.unspoken += delta;
    let cut = all ? this.unspoken.length : 0;
    if (!all) for (const match of this.unspoken.matchAll(/[.!?:]\s|\n/g)) cut = match.index + match[0].length;
    const ready = this.unspoken.slice(0, cut).trim();
    this.unspoken = this.unspoken.slice(cut);
    if (!/[\p{L}\p{N}]/u.test(ready)) return;
    this.queue.push(ready);
    this.next();
  }

  stop(): void {
    this.queue = [];
    this.unspoken = "";
    this.child?.kill();
    this.child = null;
  }

  private next(): void {
    const text = this.child ? undefined : this.queue.shift();
    if (text === undefined) return;
    // The text goes in on stdin, so a reply that starts with "-" is not read as an option.
    const child = spawn("say", [], { stdio: ["pipe", "ignore", "ignore"] });
    this.child = child;
    child.on("error", () => {});
    child.on("exit", () => {
      if (this.child === child) this.child = null;
      this.next();
    });
    child.stdin?.end(text);
  }
}

const mic = new Microphone();
const speaker = new Speaker();
let agentMode = false; // LightCraft's toolbar is up and the microphone is open
let keyDown = false;
let holdTimer: NodeJS.Timeout | null = null;
let talking = false;
let toolbarWarned = false;

function sound(name: "Tink" | "Pop"): void {
  execFile("afplay", [`/System/Library/Sounds/${name}.aiff`], () => {});
}

/** The first transcription after a while takes seconds (the model is read and set up): do it before it matters. */
function warmUp(): void {
  const hum = Buffer.alloc(44 + BYTES_PER_SECOND / 2);
  for (let at = 44; at + 1 < hum.length; at += 2) hum.writeInt16LE(at % 400 < 200 ? 1000 : -1000, at);
  transcribe(hum).catch(() => {});
}

/** Run a LightCraft UI command; false when it failed (headless LightCraft has no window to show it in). */
async function appCommand(command: string, params: object): Promise<boolean> {
  try {
    // The compact tool list names the same tool differently.
    const compact = tools.some((tool) => tool.name === "command_run");
    const result = compact
      ? await mcp.callTool("command_run", { id: command, params })
      : await mcp.callTool("run_command", { command, params });
    return result.isError !== true;
  } catch {
    return false;
  }
}

/** What the voice is doing, on the page and on the line under LightCraft's toolbar. */
function showStatus(state: string, more: { transcript?: string; error?: string } = {}): void {
  broadcast({ type: "voice", state, agentMode });
  void appCommand("assistant.status", { status: state, ...more });
}

async function setAgentMode(on: boolean): Promise<void> {
  if (agentMode === on) return;
  agentMode = on;
  if (on) {
    mic.open();
    warmUp();
  } else {
    mic.close();
    speaker.stop();
    current?.abort();
    talking = false;
  }
  broadcast({ type: "voice", state: on ? READY : "Off", agentMode });
  const shown = await appCommand("assistant.toggle", { on });
  if (on && shown) void appCommand("assistant.status", { status: READY });
  if (on && !shown && !toolbarWarned) {
    toolbarWarned = true;
    console.error(
      "LightCraft did not show its toolbar: open a photo, and run the chat against the app " +
        "(LIGHTCRAFT_MCP_ARGS=mcp --connect 127.0.0.1:7980 --compact). Voice works without it.",
    );
  }
}

/** The talk key, or the page's button, went down or up. */
function onTalk(down: boolean): void {
  if (down === keyDown) return; // the helper and the focused page both report the same press
  keyDown = down;
  if (down) {
    holdTimer = setTimeout(() => {
      holdTimer = null;
      beginTalking();
    }, HOLD_MS);
  } else if (holdTimer) {
    clearTimeout(holdTimer);
    holdTimer = null;
    void setAgentMode(!agentMode); // a tap
  } else {
    void endTalking();
  }
}

function beginTalking(): void {
  talking = true;
  speaker.stop(); // talking over the reply interrupts it,
  current?.abort(); // and stops the work behind it
  mic.begin(HOLD_MS + PRE_ROLL_MS);
  // You are probably looking at LightCraft: a tick says it is listening, a pop that it stopped.
  sound("Tink");
  void setAgentMode(true).then(() => {
    if (talking) showStatus("Listening…");
  });
}

async function endTalking(): Promise<void> {
  if (!talking) return;
  talking = false;
  const wav = mic.end();
  sound("Pop");
  if (wav.length - 44 < 0.3 * BYTES_PER_SECOND) return showStatus(READY);
  showStatus("Transcribing…");
  let text: string;
  try {
    text = await transcribe(wav);
  } catch (error) {
    broadcast({ type: "error", message: `Voice: ${describeError(error)}` });
    return showStatus(READY, { error: describeError(error) });
  }
  if (talking || !agentMode) return;
  if (!text) return showStatus("Didn't catch that. Hold the key and speak");
  await voiceTurn(text);
}

/** A spoken message: the same turn a typed one gets, shown on every page and read aloud. */
async function voiceTurn(text: string): Promise<void> {
  // A reply that was just interrupted may still be finishing a tool call.
  for (let tries = 0; busy && tries < 50; tries++) await sleep(300);
  if (talking || !agentMode) return;
  if (busy) return showStatus(READY, { error: "The last request is still running. Try again in a moment." });
  busy = true;
  const abort = new AbortController();
  current = abort;
  broadcast({ type: "user", text });
  showStatus("Working…", { transcript: text });
  let said = "";
  const send = (event: PageEvent): void => {
    broadcast(event);
    if (event.type === "text") {
      said += event.delta;
      speaker.feed(event.delta);
    } else if (event.type === "tool_call") {
      said = "";
      speaker.feed("", true);
    }
  };
  let failure: string | undefined;
  try {
    await runTurn({ text, voice: true }, send, abort.signal);
    speaker.feed("", true);
  } catch (error) {
    if (!(error instanceof Anthropic.APIUserAbortError)) {
      failure = describeError(error);
      send({ type: "error", message: failure });
    }
  } finally {
    busy = false;
    if (current === abort) current = null;
    broadcast({ type: "done" });
    if (agentMode && !talking) showStatus(READY, { transcript: said.trim() || text, error: failure });
  }
}

function run(command: string, args: string[]): Promise<void> {
  return new Promise((resolve, reject) => {
    execFile(command, args, { timeout: 180_000 }, (error, _out, err) =>
      error ? reject(new Error(err.trim().split("\n").at(-1) || error.message)) : resolve(),
    );
  });
}

function onTalkKey(line: string): void {
  if (line === "ready") {
    console.log("Talk key: tap the right Option key for agent mode; hold it, speak, and let go.");
  } else if (line === "denied") {
    console.error(
      "The talk key is off: macOS has not allowed key monitoring. Turn on the app you ran `npm start` from " +
        "under System Settings > Privacy & Security > Input Monitoring, then restart. It still works on the chat page.",
    );
  } else if (line === "down" || line === "up") {
    onTalk(line === "down");
  }
}

async function startTalkKey(): Promise<void> {
  if (process.platform !== "darwin" || process.env.GLOBAL_TALK_KEY === "off") return;
  if (!TALK_KEY_HELPER) {
    try {
      const source = await stat(TALK_KEY_SOURCE);
      const built = await stat(TALK_KEY_BIN).catch(() => null);
      if (!built || built.mtimeMs < source.mtimeMs) {
        await mkdir(path.dirname(TALK_KEY_BIN), { recursive: true });
        await run("swiftc", ["-O", TALK_KEY_SOURCE, "-o", TALK_KEY_BIN]);
      }
    } catch (error) {
      console.error(`Global talk key is off: could not build tools/talk-key.swift (${describeError(error)}).`);
      return;
    }
  }
  // Its stdin stays open to this process, and it exits when that closes.
  const child = spawn(TALK_KEY_HELPER ?? TALK_KEY_BIN, [], { stdio: ["pipe", "pipe", "inherit"] });
  talkKey = child;
  child.on("error", (error) => console.error(`Global talk key is off: ${error.message}`));
  if (child.stdout) createInterface({ input: child.stdout }).on("line", onTalkKey);
}

function handleEvents(res: ServerResponse): void {
  res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
  res.write(": connected\n\n");
  pages.push(res);
  res.on("close", () => {
    const at = pages.indexOf(res);
    if (at >= 0) pages.splice(at, 1);
  });
}

async function readBody(req: IncomingMessage, limit: number, tooLarge: string): Promise<Buffer> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of req) {
    size += (chunk as Buffer).length;
    if (size > limit) throw new Error(tooLarge);
    chunks.push(chunk as Buffer);
  }
  return Buffer.concat(chunks);
}

async function readJson(req: IncomingMessage): Promise<unknown> {
  const body = await readBody(req, MAX_BODY_BYTES, "The attachment is too large (limit about 55 MB).");
  return body.length > 0 ? JSON.parse(body.toString("utf8")) : {};
}

/** Whisper writes sounds as "[BLANK_AUDIO]" or "(wind blowing)"; those are not words the user said. */
function cleanTranscript(raw: string): string {
  const text = raw
    .replace(/\[[^\]]*\]|\([^)]*\)/g, " ")
    .replace(/\s+/g, " ")
    .trim();
  return /^you\W*$/i.test(text) ? "" : text; // what Whisper makes of a breath or a key click
}

/** Whether the page's 16-bit WAV holds nothing louder than room noise; Whisper invents words for silence. */
function isSilent(wav: Buffer): boolean {
  const QUIET = 500; // of 32768
  for (let at = 44; at + 1 < wav.length; at += 2) {
    if (Math.abs(wav.readInt16LE(at)) > QUIET) return false;
  }
  return true;
}

/** Turns a 16 kHz mono WAV recording into text with whisper.cpp, on this machine. */
async function transcribe(wav: Buffer): Promise<string> {
  if (isSilent(wav)) return "";
  try {
    await access(WHISPER_MODEL);
  } catch {
    throw new Error(`The Whisper model is missing: ${WHISPER_MODEL}. See "Voice" in the README.`);
  }
  const dir = await mkdtemp(path.join(os.tmpdir(), "lightcraft-voice-"));
  const file = path.join(dir, "speech.wav");
  try {
    await writeFile(file, wav);
    const args = ["-m", WHISPER_MODEL, "-f", file, "-nt", "-np", "--prompt", WHISPER_PROMPT];
    const stdout = await new Promise<string>((resolve, reject) => {
      execFile(WHISPER_CLI, args, { timeout: 60_000, maxBuffer: 1 << 20 }, (error, out, err) => {
        if (!error) return resolve(out);
        if ((error as NodeJS.ErrnoException).code === "ENOENT") {
          return reject(new Error("whisper-cli was not found. Install it with `brew install whisper-cpp`, or set WHISPER_CLI."));
        }
        reject(new Error(`whisper-cli failed: ${err.trim().split("\n").at(-1) || error.message}`));
      });
    });
    return cleanTranscript(stdout);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
}

function json(res: ServerResponse, status: number, body: unknown): void {
  res.writeHead(status, { "content-type": "application/json" });
  res.end(JSON.stringify(body));
}

/** This server runs tools on the user's machine: refuse requests another website makes to it. */
function sameOrigin(req: IncomingMessage): boolean {
  const origin = req.headers.origin;
  return origin === undefined || origin === `http://localhost:${PORT}` || origin === `http://127.0.0.1:${PORT}`;
}

async function handleChat(req: IncomingMessage, res: ServerResponse): Promise<void> {
  const request = (await readJson(req)) as ChatRequest;
  if (busy) return json(res, 409, { error: "A reply is still in progress." });
  busy = true;
  res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
  const send = (event: PageEvent): void => void res.write(`data: ${JSON.stringify(event)}\n\n`);
  const abort = new AbortController();
  current = abort;
  res.on("close", () => abort.abort());
  try {
    await runTurn(request, send, abort.signal);
  } catch (error) {
    if (!(error instanceof Anthropic.APIUserAbortError)) send({ type: "error", message: describeError(error) });
  } finally {
    busy = false;
    if (current === abort) current = null;
    send({ type: "done" });
    res.end();
  }
}

async function route(req: IncomingMessage, res: ServerResponse): Promise<void> {
  const url = new URL(req.url ?? "/", `http://${req.headers.host ?? "localhost"}`);
  if (!sameOrigin(req)) return json(res, 403, { error: "cross-origin requests are refused" });

  if (req.method === "GET" && url.pathname === "/") {
    res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
    res.end(await readFile(path.join(ROOT, "public", "index.html")));
  } else if (req.method === "GET" && url.pathname === "/api/events") {
    handleEvents(res);
  } else if (req.method === "GET" && url.pathname === "/api/state") {
    json(res, 200, { model: MODEL, tools: tools.map((tool) => tool.name), preview: await currentPreview(), agentMode });
  } else if (req.method === "POST" && url.pathname === "/api/chat") {
    if (!req.headers["content-type"]?.startsWith("application/json")) {
      return json(res, 415, { error: "send application/json" });
    }
    await handleChat(req, res);
  } else if (req.method === "POST" && url.pathname === "/api/talk") {
    // The page's Talk button and its own key handler: the same as the talk key.
    onTalk((await readJson(req) as { down?: boolean }).down === true);
    json(res, 200, { agentMode });
  } else if (req.method === "POST" && url.pathname === "/api/stop") {
    speaker.stop();
    current?.abort();
    json(res, 200, {});
  } else if (req.method === "POST" && url.pathname === "/api/reset") {
    if (busy) return json(res, 409, { error: "A reply is still in progress." });
    messages = [];
    mcp.close();
    await startLightcraft();
    json(res, 200, { preview: await currentPreview() });
  } else {
    json(res, 404, { error: "not found" });
  }
}

try {
  await startLightcraft();
} catch (error) {
  console.error(`Could not start LightCraft's MCP server (${CLI} ${CLI_ARGS.join(" ")}): ${describeError(error)}`);
  console.error("Build it with `cargo build --release -p lightcraft-cli`, or set LIGHTCRAFT_CLI.");
  process.exit(1);
}

createServer((req, res) => {
  route(req, res).catch((error: unknown) => {
    if (res.headersSent) res.end();
    else json(res, 500, { error: describeError(error) });
  });
}).listen(PORT, "127.0.0.1", () => {
  console.log(`LightCraft chat: http://localhost:${PORT}  (model ${MODEL}, ${tools.length} tools)`);
  void startTalkKey();
});

process.on("SIGINT", () => {
  talkKey?.kill();
  mic.close();
  speaker.stop();
  mcp.close();
  process.exit(0);
});
