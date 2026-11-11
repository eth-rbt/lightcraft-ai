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

## Voice

Hold the **right Option** key (or the "Hold to talk" button), speak, and let go. The key works in any
app, so you can stay in LightCraft; the chat page only has to be open. Your words are sent as a normal
message and the reply is read aloud. Everything runs on this machine: no audio leaves it.

```
hold key -> browser records -> POST /api/transcribe -> whisper-cli -> text -> /api/chat as usual
reply text -> the browser's speech synthesis (the macOS system voice)
```

Set up once:

```sh
brew install whisper-cpp
mkdir -p models
curl -L -o models/ggml-small.en.bin https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin
```

- **The key in other apps** comes from `tools/talk-key.swift`, which the server builds (it needs the
  Xcode command line tools) and runs. macOS asks once for Input Monitoring permission for the app you ran
  `npm start` from: allow it under System Settings > Privacy & Security > Input Monitoring and restart
  that app. The helper only listens, and only for modifier keys, so it never sees what you type.
  A tick plays when it starts listening and a pop when it stops. Without the permission the key still
  works while the page is focused. `GLOBAL_TALK_KEY=off` turns the helper off.
- Click the page once after loading it: a browser will not speak, and may not open the microphone, on a
  page you have not touched. Keep its tab open; another window may cover it.
- Pressing the key while a reply is being read or worked on interrupts it. Esc only silences the voice.
- A spoken message carries a note asking the model for short spoken sentences in place of the written
  action log (`VOICE_NOTE` in `src/server.ts`). Typed messages are unchanged.
- The microphone stays open after the first use, so later presses don't lose the first word.
- `WHISPER_MODEL` (default `models/ggml-small.en.bin`) and `WHISPER_CLI` (default `whisper-cli`) pick
  another model or binary. The default model is English only.
- The voice is the browser's default speech-synthesis voice.
