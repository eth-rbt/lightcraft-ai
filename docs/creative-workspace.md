# Creative workspace

Open a photo and click **Lc** to reveal the creative toolbar. Opening it starts the
voice assistant on Windows; clicking Lc again closes the toolbar, hides annotations,
stops the microphone and returns to the regular editor. **Pause voice** stops the
connection while keeping the drawing tools open.

The toolbar provides Pencil, Highlight, whole-stroke Eraser, Text, four ink colours,
four pencil widths, and separate annotation Undo/Redo/Clear controls. Choose
**Edit photo** to use the normal sliders and filmstrip. Drawing mode captures input
over the editor so a stroke cannot accidentally move a photo slider. Escape returns
to Edit photo; Ctrl/Cmd+Z and Shift+Ctrl/Cmd+Z undo/redo ink while drawing.

Marks belong to the active photo in its library. Switching photos hides the previous
photo's marks; returning restores them. Coordinates follow the displayed photo when
it is zoomed or resized. Annotations are session-local: closing the app discards them.
They are discussion aids and are not written into originals or photo exports.

## Agent mode on macOS

On macOS the voice and the model live outside the app, in
[`tools/lightcraft-chat`](../tools/lightcraft-chat/README.md): a small local server that calls Claude and
drives LightCraft through every MCP tool. Speech is transcribed on the machine (Whisper) and the reply is
read by the system voice, so no audio leaves it.

- **Tap the right Option key** (in any app) to open this toolbar and the microphone; tap again to close
  both and return to the regular editor.
- **Hold it**, speak, and let go to talk to the agent. Holding it with the toolbar closed opens it first.
  Holding it while the agent is answering interrupts the answer.
- The line under the toolbar says what the voice is doing and shows what it heard, then the reply.
- The drawing tools are yours to click as usual. The model sees your marks when it takes a screenshot of
  the window, so you can circle something and say "fix this".

Each edit the agent makes is replayed on the real sliders: the Edit panel opens the slider's section,
highlights the slider and draws it travelling from its old value to the new one, one slider after
another (up to eight, about 3.6 seconds in all). The edit itself has already been applied, as one undo
step per tool call, so the replay changes nothing; any key, click or edit of yours ends it. The sliders a
step moved stay marked with their old value until the next edit.

The server reaches the toolbar through two UI commands: `assistant.toggle {on}` opens or closes it
without flipping a state it cannot see, and `assistant.status {status?, transcript?, error?}` sets its
status line. The Send box and Pause voice button belong to the Windows voice below and do nothing here.

## Spoken editing on Windows

The Windows desktop app uses native microphone/speaker audio and the OpenAI Realtime
API, with the education assistant's default model and voice. Add
`OPENAI_API_KEY_ETHAN=your-key` (or `OPENAI_API_KEY=your-key`) to a local `.env`
in the project folder or beside the executable. The file is ignored by Git. Simple
unquoted, single-quoted and double-quoted values are supported. Reconnect reloads it;
there is no need to rebuild after changing the key. Process environment variables
take precedence over the file; within either source, `OPENAI_API_KEY_ETHAN` wins.
Model and voice overrides remain process environment variables:
`OPENAI_REALTIME_MODEL` (default `gpt-realtime-2.1`) and `OPENAI_VOICE` (default `marin`).
The key must have access to the configured model. Credentials are never saved in the
catalog. Drawing works without a key. Native voice is currently Windows-only;
the web and other desktop platforms retain drawing tools.

While connected, microphone audio and cropped images of this editor are sent to the
provider. The toolbar and typed draft are excluded from images; a submitted text
message is sent as feedback. No other application or desktop is captured. Closing
the toolbar stops sharing. Switching photos restarts the conversation so a tool call
for the previous picture cannot act on the new picture.

Ask for an adjustment, such as “raise the exposure a little,” or ask the assistant to
mark an area for discussion. It can adjust exposure, contrast, highlights, shadows,
whites, blacks, vibrance, saturation, texture, clarity, dehaze, temperature and tint.
Each adjustment opens its normal edit section, highlights its actual slider and
moves it over about 1.2 seconds. The displayed photo changes through the usual
non-destructive develop pipeline. One adjustment creates one photo undo step, and
Auto Sync does not spread it to other selected photos.

Speak, type and send feedback, or use the editor to interrupt a running adjustment.
The slider stops at the current value, allowing a new instruction to change course.
The model receives a fresh editor image and current control values before continuing.
Normal photo Undo reverses its edits. The assistant cannot delete, import, export,
or edit another photo through these tools.

Use headphones if speaker playback feeds back into the microphone: native acoustic
echo cancellation is not implemented. Provider-free tests cover connection lifetime,
speech/text interruption, actual develop changes, stale-action rejection, and photo
isolation. Account access, physical audio devices and a live paid conversation still
need verification on the user's setup.

## Implementation and control channel

UI commands: `assistant.toggle` (`on`: ask for a state), `assistant.status`, `assistant.voice`, `assistant.tool` (`tool` is
`interact`, `pen`, `highlighter`, `eraser` or `text`), `assistant.undo`,
`assistant.redo`, `assistant.clear`, `assistant.send` (`text`) and `assistant.stop`.
The logo widget is `assistant:logo`; tool widgets use `assistant:<tool>`.
These use the existing JSON-lines control channel and menu command routing.

UI/ink/actions live in `crates/ui-egui/src/assistant/`; the native audio and websocket
transport live in `apps/lightcraft/src/voice/`. Images, queues, marks and tool batches
are bounded. Revisions and photo identities reject stale operations.

This is an extension beyond the Lightroom parity checklist; it does not change its
feature counts. The transport follows the provider's
[Realtime conversation protocol](https://developers.openai.com/api/docs/guides/realtime-conversations).
