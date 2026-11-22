# v2t

Voice typing for Windows 10 and 11. Hold your dictation shortcut and wait for the short tone to finish before speaking. Release the shortcut keys to type the transcript into the active app. Keep the cursor in the same text field until the transcript appears.

1. [Build v2t](#build-and-test), then open `publish/V2t.exe`. The v2t icon in the Windows system tray opens Settings.
2. Select **Local**, **OpenAI**, or **OpenRouter**. Settings changes apply automatically. For Local, install the model in Settings if needed. For a cloud provider, enter that provider's API key and then leave the field or press Enter to apply it.
3. Place the cursor in a text field and hold **Ctrl+Shift** to dictate. You can change the default under **Dictation shortcut** in Settings. **Use shortcut** applies your selection immediately. Pressing an extra key during dictation cancels the recording. If the target changes, recover the result with **Copy last transcript** from the tray menu.

**Local** uses Typhoon Whisper Turbo Q5 for Thai and mixed Thai–English. English and all-language detection use Whisper Large V3 Turbo Q5. Model setup downloads about 547 MiB per model, and an existing stock Whisper model can handle Thai while Typhoon is being installed. Once installed, Local transcribes offline. The OpenAI option uses `gpt-transcribe`, and OpenRouter uses `openai/whisper-large-v3-turbo`. Cloud modes send the current recording to the selected provider. Settings encrypt each saved key for the current Windows account.

Choose **Thai + English (mixed)** to dictate both languages in one sentence. Local accepts results labeled Thai or English. OpenAI receives both languages as hints. OpenRouter Whisper uses its automatic detection. **Detect automatically (all languages)**, **Thai**, and **English** are also available.

### Local speech backup

With **Local** and **Use MAI-Transcribe-2 when Local is busy or fails** selected, v2t tries local speech recognition first during normal use. If the local model fails or times out, v2t sends that recording once to OpenRouter's MAI-Transcribe-2. v2t skips silence and very quiet background noise before sending audio to any model. Empty local results and recordings rejected by the mixed-language check stay local.

v2t automatically chooses MAI for the next recording when it detects a foreground game, less than 2 GiB of available RAM, less than 1.5 GiB of free NVIDIA VRAM, or GPU utilization of 50% or higher on two consecutive readings. Game detection uses the Windows exclusive Direct3D fullscreen signal and known game library paths. In this mode v2t releases the local speech and correction workers and still applies the Dictionary. Local models load again when needed after three healthy readings. This routing uses the same OpenRouter key and backup checkbox.

Use the OpenRouter key in Settings, an `OPENROUTER_API_KEY` or `OPENROUTER_KEY_TOKEN` environment variable, or the same field in `.env` beside the executable. A workspace build in `publish` also reads the project’s `.env`. The key file is read when v2t starts or applies a settings change.

## Dictionary and AI correction

Open **Dictionary & AI** from the v2t tray menu or Settings. Add a pair such as `โอเพ่นเราเตอร์` → `OpenRouter` and save it. v2t applies saved replacements before inserting each transcript.

Enable **Use local AI to correct dictation** to send the mapped text to Typhoon 2.5 4B on this computer for spelling and punctuation cleanup. The model download is about 2.3 GiB. If the model is unavailable, takes too long, or changes a protected replacement, v2t inserts the dictionary result.

### Usage log

v2t writes usage metadata to `%LOCALAPPDATA%\v2t\usage\yyyy-MM-dd.csv`, with one file per local calendar date. Each speech transcription or AI correction request records its start time, stage, provider, model, outcome, audio duration, elapsed time, billed seconds, and API cost in USD. v2t keeps today and the six previous dates. It deletes older daily files at startup and at local midnight, or when Windows wakes after midnight. If a legacy `usage.csv` exists, v2t drops its expired rows at the same time.

OpenRouter costs and billed seconds come from its response. Local requests record zero API cost. When OpenAI returns a successful transcript without a cost figure, v2t estimates it from the submitted audio at $0.0045 per minute and labels it `estimated`. Unknown costs are blank.

## Build and test

The executable requires the .NET 10 Desktop Runtime on Windows x64. To build it with the .NET 10 SDK, run these commands from the project folder.

```powershell
dotnet run --project tests/V2t.Tests/V2t.Tests.csproj
dotnet publish app/V2t.Windows/V2t.Windows.csproj -c Release -r win-x64 --self-contained false -o publish
```
