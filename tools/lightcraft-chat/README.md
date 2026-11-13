# LightCraft Chat

A local chat page where Claude edits photos through LightCraft's MCP server.

```
browser page  <->  src/server.ts  <->  Claude API
                        |
               lightcraft-cli mcp   (child process, stdio)
```

## Run

```sh
cargo build --release -p lightcraft-cli   # once, from the repository root
cd tools/lightcraft-chat
cp .env.example .env        # then put your ANTHROPIC_API_KEY in it
npm install
npm start                   # http://localhost:8787
```

## Settings (`.env`)

| Variable | Default | Meaning |
|---|---|---|
| `ANTHROPIC_API_KEY` | none | Required |
| `MODEL` | `claude-opus-5-5` | Any Claude model id |
| `PORT` | `8787` | The server listens on 127.0.0.1 only |
| `LIGHTCRAFT_CLI` | `../../target/release/lightcraft-cli` | Path to the CLI |
| `LIGHTCRAFT_MCP_ARGS` | `mcp --demo --compact` | See below |

`LIGHTCRAFT_MCP_ARGS` picks what the chat drives:

- `mcp --demo --compact`: a throwaway in-memory demo library.
- `mcp --library /path/to/library --compact`: a real library; edits are saved.
- `mcp --connect 127.0.0.1:7980 --compact`: the running desktop app (start it with `--control 7980`).

The system prompt is `system-prompt.txt`; restart, or press Reset on the page, after editing it.

## How the model sees photos

- Tool results that contain an image (`render_photo`, `render_preview`) are passed to the model as images.
- An attached photo is saved to `uploads/`, imported into LightCraft, and the model gets LightCraft's
  1024 px render of it, so raw files work too.
- The "Current photo" panel re-renders the active photo after every round of tool calls.

Reset clears the conversation and restarts the LightCraft process.

## Voice and agent mode

The **right Option** key works in any app, so you can stay in LightCraft. The chat page does not have
to be open.

- **Tap** it: agent mode turns on. LightCraft shows its creative toolbar (annotation tools and a status
  line) and the microphone opens. Tap again: the toolbar goes away and the microphone closes.
- **Hold** it, speak, and let go: your words are sent as a normal message and the reply is read aloud.
  Holding it outside agent mode turns agent mode on first.
- Holding it while a reply is being read or worked on interrupts that reply.

A tick plays when it starts listening and a pop when it stops. Everything about the voice runs on this
machine: no audio leaves it. The words you said, as text, go to Claude like a typed message.

```
talk key (tools/talk-key.swift) -> server: tap = agent mode, hold = talk
microphone (ffmpeg) -> whisper-cli -> text -> the same turn a typed message gets
reply text -> say (the macOS system voice)
LightCraft: assistant.toggle shows the toolbar, assistant.status its status line
```

Set up once:

```sh
brew install whisper-cpp ffmpeg
mkdir -p models
curl -L -o models/ggml-small.en.bin https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin
```

- **Permissions.** macOS asks twice, both for the app you ran `npm start` from (your terminal): Input
  Monitoring for the key, and Microphone the first time agent mode opens. Allow them under System
  Settings > Privacy & Security and restart that app. The key helper only listens, and only for modifier
  keys, so it never sees what you type. Without Input Monitoring the key still works while the chat page
  is focused, and so does its "Hold to talk" button.
- **The toolbar** needs the chat to drive the running app
  (`LIGHTCRAFT_MCP_ARGS=mcp --connect 127.0.0.1:7980 --compact`) with a photo open. Voice works without it.
- **What you see in LightCraft.** Each edit the model makes is replayed on the real sliders: the slider
  is highlighted and travels from its old value to the new one
  ([creative workspace](../../docs/creative-workspace.md)).
- A spoken message carries a note asking the model for short spoken sentences in place of the written
  action log (`VOICE_NOTE` in `src/server.ts`). Typed messages are unchanged.
- The microphone is open only while agent mode is on.
- Right Option pressed together with another key still counts as a tap.
- `WHISPER_MODEL` (default `models/ggml-small.en.bin`) and `WHISPER_CLI` (default `whisper-cli`) pick
  another model or binary. The default model is English only. `VOICE_INPUT` is the ffmpeg input
  (default `-f avfoundation -i :default`, the system's input device). `GLOBAL_TALK_KEY=off` turns the
  key helper off.
- The voice is the system voice (`say`). macOS only.
