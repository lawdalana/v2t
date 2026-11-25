# v2t

Voice typing for Windows and Linux, with a macOS 2.4 development build. v2t is written in Rust and starts the local speech and correction engines as separate processes. Download released packages from [GitHub Releases](https://github.com/lawdalana/v2t/releases).

## Windows desktop

Windows 10 and 11 use the tray app. Hold your dictation shortcut and wait for the short tone to finish before speaking. Release the shortcut keys to type the transcript into the active app. Keep the cursor in the same text field until the transcript appears.

1. Open `V2t.exe` from the release archive or the `publish` directory after a [source build](#build-and-test). The v2t icon in the Windows system tray opens Settings.
2. Select **Local**, **OpenAI**, or **OpenRouter**, then click **Save settings**. For Local, install the model in Settings if needed. For a cloud provider, enter its API key before saving.
3. Place the cursor in a text field and hold **Ctrl+Shift** to dictate. Enter a different combination in **Hold to dictate**, for example `Ctrl+F9`, and save to change it. Pressing an extra key during dictation cancels the recording. If the target changes, recover the result with **Copy last transcript** from the tray menu.

**Local** uses Typhoon Whisper Turbo Q5 for Thai and mixed Thai–English. English and all-language detection use Whisper Large V3 Turbo Q5. Model setup downloads about 547 MiB per model, and an existing stock Whisper model can handle Thai while Typhoon is being installed. Once installed, Local transcribes offline. The OpenAI option uses `gpt-transcribe`, and OpenRouter uses `openai/whisper-large-v3-turbo`. Cloud modes send speech segments to the selected provider during recording. Settings encrypt each saved key for the current Windows account.

Choose **Thai + English (mixed)** to dictate both languages in one sentence. Local accepts results labeled Thai or English. OpenAI receives both languages as hints. OpenRouter Whisper uses its automatic detection. **Detect automatically (all languages)**, **Thai**, and **English** are also available.

### Transcription while recording

v2t starts processing each speech segment after about one second of silence and keeps listening. Once a segment reaches 10 seconds, a half-second pause is enough. Continuous speech is split at 12 seconds. v2t processes any remaining speech when recording ends, then combines the text in the order you spoke it. This applies to Windows and macOS desktop dictation and the `v2t record` command.

### Local speech backup

With **Local** and **Use MAI-Transcribe-2 when Local is busy or fails** selected, v2t tries local speech recognition first during normal use. If local recognition fails or times out on a segment, v2t sends that segment once to OpenRouter's MAI-Transcribe-2. v2t skips silence and very quiet background noise before sending audio to any model. Empty local results and segments rejected by the mixed-language check stay local.

v2t automatically chooses MAI for the next recording when it detects a foreground game, less than 2 GiB of available RAM, less than 1.5 GiB of free NVIDIA VRAM, or GPU utilization of 50% or higher on two consecutive readings. Game detection uses the Windows exclusive Direct3D fullscreen signal and known game library paths. In this mode v2t releases the local speech and correction workers and still applies the Dictionary. v2t preloads installed, enabled local models when it starts. If resource pressure pauses the models before the first local transcription attempt, v2t tries once more in the background after three healthy readings. After that, paused models load on demand. This routing uses the same OpenRouter key and backup checkbox.

Use the OpenRouter key in Settings, an `OPENROUTER_API_KEY` or `OPENROUTER_KEY_TOKEN` environment variable, or the same field in `.env` beside the executable. A workspace build in `publish` also reads the project’s `.env`. The key file is read when v2t starts or applies a settings change.

MAI backup requests require OpenRouter Zero Data Retention and deny provider data collection. Set **All other models → ZDR only** on the OpenRouter key's guardrail or in account privacy settings to exclude Safety Retention endpoints. If OpenRouter rejects the request, v2t reports the error without retrying with weaker settings. [OpenRouter ZDR settings](https://openrouter.ai/docs/guides/features/zdr#allowing-or-excluding-safety-retention)

## Dictionary and AI correction

Open **Dictionary & AI** from the v2t tray menu or Settings. Enter a pair such as `โอเพ่นเราเตอร์` → `OpenRouter`, click **Add**, then click **Save dictionary**. v2t applies saved replacements before inserting each transcript.

Use **Export…** to save the dictionary as a JSON file for backup or transfer between Windows and macOS. **Import…** adds new spoken terms and keeps your existing replacements for matching terms. Your local AI preference stays the same. On Windows, click **Save dictionary** to apply the imported rows. On macOS, imports are saved immediately.

On Windows, enable **Use local AI to correct dictation** in **Dictionary & AI**. On macOS, choose a model under **AI correction**. v2t passes the mapped text to the selected model for spelling and punctuation cleanup. If the model is unavailable, takes too long, or changes a protected replacement, v2t uses the dictionary result.

### Usage log

v2t writes usage metadata to `%LOCALAPPDATA%\v2t\usage\yyyy-MM-dd.csv`, with one file per local calendar date. Each speech transcription or AI correction request records its start time, stage, provider, model, outcome, audio duration, elapsed time, billed seconds, and API cost in USD. v2t keeps today and the six previous dates. It deletes older daily files at startup and at local midnight, or when Windows wakes after midnight. If a legacy `usage.csv` exists, v2t drops its expired rows at the same time.

OpenRouter costs and billed seconds come from its response. Local requests record zero API cost. When OpenAI returns a successful transcript without a cost figure, v2t estimates it from the submitted audio at $0.0045 per minute and labels it `estimated`. Unknown costs are blank.

## Linux command line

Extract the x86-64 package and run `./v2t` from its directory. The package includes CPU builds of `whisper-server` and `llama-server`. Local speech needs an AVX2-capable x86-64 CPU. The package was tested on Debian 12. Local AI correction also needs the OpenSSL 3 and OpenMP runtimes. Microphone recording needs `parec` from `pulseaudio-utils` and a PulseAudio or PipeWire PulseAudio session.

```bash
./v2t setup --language th+en
./v2t transcribe recording.wav
./v2t record --seconds 8
```

The Linux binary prints the transcript to stdout. WAV input is 16 kHz, mono, signed 16-bit PCM, up to five minutes. Use `--language th|en|auto|th+en` on transcription or recording to choose the model and language. Without a language option, v2t uses mixed Thai and English.

To enable local AI correction, run `./v2t setup --correction`, then add `--correct` to `record` or `transcribe`. v2t uses `$XDG_DATA_HOME/v2t` for downloaded models and usage logs, or `~/.local/share/v2t` by default. Optional dictionary entries use `$XDG_CONFIG_HOME/v2t/correction.json`, or `~/.config/v2t/correction.json`, with the same `Entries` format as on Windows. Source builds can use `whisper-server` and `llama-server` from the binary's directory or PATH. Set `V2T_WHISPER_SERVER` or `V2T_LLAMA_SERVER` to an absolute executable path to select a different backend.

Use `--provider openai` or `--provider openrouter` to opt into cloud transcription with the respective `OPENAI_API_KEY` or `OPENROUTER_API_KEY` environment variable. In Local mode, add `--mai-backup` to enable the same ZDR-requested MAI backup on failure. Use the OpenRouter key with **ZDR only** configured as described above.

## macOS desktop (2.4 development build)

Build on macOS 12 or later with Rust and the Xcode command-line tools. Install `whisper-server` for the Mac's architecture. With Homebrew already installed, `brew install whisper.cpp` provides it. See the [Homebrew formula](https://formulae.brew.sh/formula/whisper.cpp).

```bash
bash app/v2t-macos/build.sh
open publish/V2t.app
```

1. In the v2t window, choose your **Dictation language**, check any **Hold to dictate** shortcuts you want, then click **Save Settings**. Clearing every checkbox disables the shortcuts. Choose **Install Speech Model** if you need the local model for that language.
2. Hold **Control + Shift + Space**, the default, or any checked shortcut to record. The first time, allow microphone access at the macOS prompt, then hold the shortcut again. **Right Option + Space** works in the v2t window. Use **Allow Accessibility…** to enable it in other apps under **System Settings → Privacy & Security → Accessibility**. **Right Command + Right Option** works while the v2t window has keyboard focus.
3. Release the shortcut to finish recording and copy the result. A short sound plays when the transcript is ready. Press **⌘V** in your destination app. You can also use **Start Recording** and **Stop and Copy** in the window.

**Command + Option + Space** accepts either side of both modifier keys and can be used from other apps. macOS also assigns this combination to opening Finder search by default. [Apple Spotlight shortcuts](https://support.apple.com/en-gb/guide/mac-help/mh26783/26/mac/26)

Use **Copy** next to **Transcript** or **Copy Last Transcript** in the v2t menu bar menu to copy the latest result again.

Open **Dictionary…** from the v2t window or menu bar to add spoken phrases and their replacements. Select a saved rule to update or remove it. Use **Import…** and **Export…** to transfer dictionary rules as JSON. Changes are saved immediately and apply to the next transcription. English replacements get a space where they touch adjacent words.

Click **Install Local AI** to download the Typhoon 2.5 4B correction model (about 2.3 GiB) and `llama-server` if needed. The automatic engine download requires macOS 13.3 or later. On older macOS versions you can provide a compatible `llama-server` using `V2T_LLAMA_SERVER`. After installation, choose **Typhoon 2.5 4B (local)** under **AI correction** and click **Save Settings**.

To use LiteLLM for correction, pipe the gateway key to `setup-litellm` and provide the HTTPS base URL and model alias as arguments. The command tests the selected model and stores the key in macOS Keychain. It then selects **LiteLLM** under **AI correction**. Reopen the app to load the updated settings. v2t sends the text and protected dictionary terms to the gateway after local speech recognition and dictionary replacement. [LiteLLM virtual key documentation](https://docs.litellm.ai/docs/proxy/api_key_auth)

```bash
printf '%s\n' "$LITELLM_API_KEY" | publish/V2t.app/Contents/MacOS/V2t setup-litellm "$LITELLM_BASE_URL" "$LITELLM_MODEL"
```

Add `--reasoning-effort low` to request low reasoning effort on models that support it.

To start recording from Spotlight, keep **V2t Record.app** beside **V2t.app** in a Spotlight-indexed folder such as `~/Applications`. Press **⌘Space**, search for **V2t Record**, and press **Return**. In the v2t window, click **Stop and Copy** to finish.

The v2t menu bar icon also lets you start or stop recording and reopen the window with **Settings…**. The app runs in your user session and needs Microphone permission. You can grant it under **System Settings → Privacy & Security → Microphone**. On macOS 12, use **System Preferences → Security & Privacy → Privacy → Microphone**.

The CLI is available inside the bundle at `publish/V2t.app/Contents/MacOS/V2t`. If your `whisper-server` is installed outside the usual executable directories, you can start v2t from Terminal with `V2T_WHISPER_SERVER` set to its absolute path. Set `V2T_BUNDLE_WHISPER_SERVER` when building to include a compatible executable in the signed app. Later rebuilds reuse the bundled executable. Quit v2t before rebuilding it. The build script signs the app locally for development.

## Start V2T At Startup

On Windows, open **Settings** from the tray and tick **Start V2T At Startup**. This applies immediately in both Windows editions. The next time you sign in, Windows launches v2t in the tray and prepares the installed local models.

Linux and macOS use the same CLI for dictation and login settings. Run `v2t settings`, set **Start V2T At Startup**, and confirm. On Linux, the dialog uses `zenity`. On macOS, it uses the built-in system dialog. The CLI can manage the same preference.

```bash
v2t startup enable
v2t startup status
v2t startup disable
```

Linux uses the desktop session's autostart folder and macOS uses a per-user LaunchAgent. At login, v2t prepares installed models and keeps them available to `record` and `transcribe` through a private local socket. Audio capture starts with `record`. Run `v2t stop-background` to stop the current worker. The startup checkbox controls future logins. If you move the executable, open Settings from its new location and enable startup there.

For a macOS source build, use Rust and the Xcode command-line tools. The CLI looks for macOS builds of `whisper-server` and `llama-server` beside it, on PATH, or in the usual Homebrew binary directories. It can also use the `llama-server` installed by **Install Local AI** or `v2t setup --correction`. `--correct` uses the selected macOS correction provider while honoring the CLI's Dictionary preference. Use `ffmpeg` with AVFoundation and allow your terminal to use the microphone for `v2t record`. The default model and configuration directory is `~/Library/Application Support/v2t`.

## Build and test

Build with Rust 1.93 or newer. On Windows x64, install the Microsoft C++ Build Tools and Windows SDK, then run these commands from the project folder.

```powershell
cargo test --locked --manifest-path app/v2t-rust/Cargo.toml
cargo build --release --locked --manifest-path app/v2t-rust/Cargo.toml
New-Item -ItemType Directory -Force publish | Out-Null
Copy-Item app/v2t-rust/target/release/V2t.exe publish/V2t.exe
```

Close v2t from the tray before replacing the executable. Pass `--background` in a Windows Startup shortcut to launch directly into the tray. Existing settings, DPAPI-encrypted API keys, dictionaries, and installed models carry over from the .NET version under `%LOCALAPPDATA%\v2t`.

On Linux x86-64, build and test with a C compiler installed.

```bash
cargo test --locked --manifest-path app/v2t-rust/Cargo.toml
cargo build --release --locked --manifest-path app/v2t-rust/Cargo.toml
app/v2t-rust/target/release/V2t --version
```
