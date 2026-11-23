using System.ComponentModel;
using System.Diagnostics;
using Microsoft.Win32;
using V2t.Core;

namespace V2t.Windows;

internal sealed class TrayApp : ApplicationContext
{
    private const int MinimumWaveBytes = 4_800;
    private const decimal OpenAiTranscriptionUsdPerMinute = 0.0045m;
    private static readonly string CorrectionFile = Path.Combine(SettingsStore.DirectoryPath, "correction.json");
    private static readonly string LegacyUsageFile = Path.Combine(SettingsStore.DirectoryPath, "usage.csv");
    private readonly SettingsForm form;
    private readonly NotifyIcon tray;
    private readonly Icon trayIcon = AppIcon.LoadTrayIcon();
    private readonly ToolStripMenuItem copy;
    private readonly WindowsInput keyboard;
    private readonly ListeningCue listeningCue = new();
    private readonly LocalEngine local = new();
    private readonly LocalCorrectionEngine correctionEngine = new();
    private readonly WindowsResourceProbe resourceProbe = new();
    private readonly SpeechResourceRouting resourceRouting = new();
    private readonly DailyUsageLog usageLog = new(LegacyUsageFile);
    private readonly System.Threading.Timer usageCleanupTimer;
    private readonly System.Windows.Forms.Timer resourceTimer = new() { Interval = 2_000 };
    private readonly HttpClient cloud = new(new SocketsHttpHandler { AllowAutoRedirect = false })
    {
        Timeout = TimeSpan.FromSeconds(90)
    };
    private readonly CancellationTokenSource lifetime = new();

    private DictationSettings settings;
    private string? openRouterKey;
    private CorrectionPreferences corrections;
    private DictionaryForm? dictionaryForm;
    private MicrophoneRecorder? pendingRecording;
    private MicrophoneRecorder? recording;
    private FocusTarget target;
    private Task<bool>? pausedWorkers;
    private int warmingWorkers;
    private bool retryInitialWarmup = true;
    private bool transcribing;
    private bool installing;
    private bool installingCorrection;
    private bool exiting;
    private bool usageLogUnavailable;
    private string? lastText;
    private string? recordingStartError;
    private bool IsDictating => pendingRecording is not null || recording is not null || transcribing;
    private string ShortcutLabel => settings.Hotkey.Label;

    public TrayApp(bool background)
    {
        string? loadError = null;
        try
        {
            settings = SettingsStore.Load();
        }
        catch (Exception error) when (error is IOException or InvalidDataException or System.Text.Json.JsonException or
            System.Security.Cryptography.CryptographicException or FormatException or UnauthorizedAccessException)
        {
            settings = new DictationSettings(EnableMaiBackup: false);
            loadError = "Could not read saved settings. Review your provider and keys in Settings.";
        }
        try
        {
            corrections = CorrectionSettingsStore.Load(CorrectionFile);
        }
        catch (Exception error) when (error is IOException or InvalidDataException or UnauthorizedAccessException or
            ArgumentException or System.Text.Json.JsonException)
        {
            corrections = new CorrectionPreferences(new CorrectionDictionary([]), true);
            loadError ??= "Could not read the saved dictionary. Review it in Dictionary & AI.";
        }

        openRouterKey = OpenRouterKeySource.Resolve(settings.OpenRouterKey, AppContext.BaseDirectory);
        form = new SettingsForm(settings, ApplySettings);
        _ = form.Handle;
        form.InstallRequested += language => _ = InstallLocalAsync(language);
        form.DictionaryRequested += ShowDictionary;
        form.SetInstalling(false);
        listeningCue.WarmUp();
        copy = new ToolStripMenuItem("Copy last transcript", null, (_, _) => CopyTranscript()) { Enabled = false };
        var menu = new ContextMenuStrip();
        menu.Items.Add("Settings", null, (_, _) => ShowSettings());
        menu.Items.Add("Dictionary && AI…", null, (_, _) => ShowDictionary());
        menu.Items.Add(copy);
        menu.Items.Add(new ToolStripSeparator());
        menu.Items.Add("Exit", null, (_, _) => ExitThread());
        tray = new NotifyIcon { Icon = trayIcon, Text = $"v2t · Hold {ShortcutLabel} to speak", ContextMenuStrip = menu };
        tray.DoubleClick += (_, _) => ShowSettings();

        keyboard = new WindowsInput(
            settings.Hotkey,
            QueueRecording,
            () => form.BeginInvoke(() => _ = FinishRecordingAsync()),
            () => form.BeginInvoke(() => _ = FinishRecordingAsync(cancelled: true)));
        SystemEvents.SessionSwitch += ResetShortcut;
        tray.Visible = true;

        if (!background)
            ShowSettings();
        SetStatus(loadError ?? (settings.Provider == SpeechProvider.Local && !LocalEngine.Available(settings.Language)
            ? CanUseMaiBackup(settings)
                ? "Local model unavailable. MAI backup is ready."
                : "Install the local model or select a cloud provider."
            : $"Ready. Hold {ShortcutLabel} to speak."));
        resourceTimer.Tick += (_, _) => MonitorResources();
        resourceTimer.Start();
        usageCleanupTimer = new System.Threading.Timer(CleanupUsage);
        SystemEvents.PowerModeChanged += CleanupUsageOnResume;
        SystemEvents.TimeChanged += CleanupUsageOnClockChange;
        ScheduleUsageCleanup(TimeSpan.Zero);
        form.BeginInvoke(WarmInstalledModels);
    }

    private void WarmInstalledModels()
    {
        if (exiting)
            return;

        if (settings.Provider == SpeechProvider.Local && LocalEngine.Available(settings.Language))
            _ = WarmLocalAsync();
        if (corrections.EnableAi && LocalCorrectionEngine.Installed)
            _ = WarmCorrectionAsync();
    }

    private void ResetShortcut(object sender, SessionSwitchEventArgs e)
    {
        if (!exiting && form.IsHandleCreated)
            form.BeginInvoke(keyboard.ResetShortcut);
    }

    private void CleanupUsageOnResume(object sender, PowerModeChangedEventArgs e)
    {
        if (e.Mode == PowerModes.Resume)
            RescheduleUsageCleanup();
    }

    private void CleanupUsageOnClockChange(object? sender, EventArgs e) => RescheduleUsageCleanup();

    private void RescheduleUsageCleanup()
    {
        TimeZoneInfo.ClearCachedData();
        ScheduleUsageCleanup(TimeSpan.Zero);
    }

    private void CleanupUsage(object? state)
    {
        var now = DateTimeOffset.Now;
        if (!usageLog.TryCleanup(now))
            Trace.TraceWarning("v2t could not remove expired usage logs.");
        var nextDate = now.Date.AddDays(1);
        var nextMidnight = new DateTimeOffset(nextDate, TimeZoneInfo.Local.GetUtcOffset(nextDate));
        var delay = nextMidnight - DateTimeOffset.Now;
        ScheduleUsageCleanup(delay > TimeSpan.Zero ? delay : TimeSpan.Zero);
    }

    private void ScheduleUsageCleanup(TimeSpan delay)
    {
        try
        {
            usageCleanupTimer.Change(delay, Timeout.InfiniteTimeSpan);
        }
        catch (ObjectDisposedException)
        {
            // The app exited while a cleanup callback was running.
        }
    }

    private bool QueueRecording()
    {
        if (IsDictating || exiting)
            return false;

        var focused = WindowsInput.CaptureFocus();
        if (!IsDictationTarget(focused.Window))
            return false;
        if (settings.Hotkey.Key != 0 && ValidateRecordingStart(focused) is { } error)
        {
            form.BeginInvoke(() =>
            {
                if (!exiting)
                    SetStatus(error, notify: true);
            });
            return false;
        }

        form.BeginInvoke(() => _ = StartRecordingAsync(focused));
        return true;
    }

    private bool IsDictationTarget(nint window) =>
        window != 0 && !form.OwnsShortcutTarget(window) &&
        !(dictionaryForm is { IsHandleCreated: true } editor && window == editor.Handle);

    private async Task StartRecordingAsync(FocusTarget focused)
    {
        if (IsDictating || exiting)
            return;

        recordingStartError = ValidateRecordingStart(focused);
        if (recordingStartError is not null)
            return;

        MicrophoneRecorder? prepared = null;
        try
        {
            var cue = listeningCue.PlayAsync();
            prepared = new MicrophoneRecorder(startImmediately: false);
            pendingRecording = prepared;
            var resourceReason = ReadResourceReason(settings, focused.Window);
            bool safeToCapture = await cue;
            if (pendingRecording != prepared || exiting || keyboard.DictationKeysReleased())
                return;
            if (!safeToCapture)
            {
                recordingStartError = "Could not finish playing the listening sound. Try again.";
                return;
            }
            if (!focused.Matches(WindowsInput.CaptureFocus()))
            {
                recordingStartError = "Keep the cursor in the text field while starting dictation.";
                return;
            }

            prepared.Start();
            recording = prepared;
            pendingRecording = null;
            target = focused;
            if (resourceReason != SpeechResourceReason.None)
                PauseLocalWorkers();
            SetStatus("Listening…");
        }
        catch (Exception error) when (error is InvalidOperationException or Win32Exception)
        {
            if (prepared is null || pendingRecording == prepared)
                recordingStartError = "Could not start the microphone. Check the Windows input device and microphone access.";
        }
        finally
        {
            if (prepared is not null && pendingRecording == prepared)
                CancelPendingRecording();
        }
    }

    private string? ValidateRecordingStart(FocusTarget focused)
    {
        if (!IsDictationTarget(focused.Window))
            return "Place the cursor in the app where you want to type.";
        if (settings.Provider == SpeechProvider.Local && !LocalEngine.Available(settings.Language) &&
            !CanUseMaiBackup(settings))
            return "Install the local model in v2t settings.";
        if (settings.Provider != SpeechProvider.Local && string.IsNullOrWhiteSpace(ApiKeyFor(settings)))
            return $"Add your {settings.Provider} API key in v2t settings.";
        return null;
    }

    private void CancelPendingRecording()
    {
        var prepared = pendingRecording;
        pendingRecording = null;
        prepared?.Dispose();
    }

    private async Task FinishRecordingAsync(bool cancelled = false)
    {
        CancelPendingRecording();
        var startError = recordingStartError;
        recordingStartError = null;
        if (recording is not { } captured)
        {
            if (!cancelled && !exiting && startError is not null)
                SetStatus(startError, notify: true);
            return;
        }

        using var recorder = captured;
        recording = null;
        transcribing = true;
        var destination = target;
        var current = settings;
        var correctionPreferences = corrections;
        var latency = Stopwatch.StartNew();
        try
        {
            SetStatus(cancelled ? $"Ready. Hold {ShortcutLabel} to speak." : "Transcribing…");
            var wave = await recorder.StopAsync();
            if (exiting)
                return;
            if (cancelled)
                return;
            if (wave.Length < MinimumWaveBytes || !SpeechAudioGate.HasSpeech(wave))
            {
                SetStatus($"Ready. Hold {ShortcutLabel} while speaking.");
                return;
            }

            var resourceReason = ReadResourceReason(current, WindowsInput.CaptureFocus().Window);
            if (resourceReason != SpeechResourceReason.None)
                PauseLocalWorkers();

            var transcription = await TranscribeDictationAsync(current, wave, resourceReason);
            if (exiting)
                return;

            var text = DictationText.ForInsertion(transcription.Text);
            if (text.Length == 0)
            {
                SetStatus($"No speech recognized. Hold {ShortcutLabel} to try again.");
                return;
            }

            var mapped = correctionPreferences.Dictionary.Apply(text);
            text = mapped.Text;
            var correctionFallback = false;
            if (resourceReason == SpeechResourceReason.None && correctionPreferences.EnableAi &&
                LocalCorrectionEngine.Installed)
            {
                var correctionPressure = ReadResourceReason(current, WindowsInput.CaptureFocus().Window,
                    observeGpu: transcription.UsedBackup);
                if (correctionPressure != SpeechResourceReason.None)
                {
                    PauseLocalWorkers();
                }
                else
                {
                    var corrected = await TryCorrectAsync(mapped);
                    if (exiting)
                        return;
                    correctionFallback = corrected is null;
                    text = corrected ?? text;
                }
            }

            lastText = text;
            copy.Enabled = true;
            if (WindowsInput.TryInsert(destination, text))
            {
                var result = correctionFallback ? " Used the dictionary result." : "";
                var source = transcription.UsedBackup ? "MAI backup typed" : "Typed";
                SetStatus($"{source} in {latency.Elapsed.TotalSeconds:F2} s.{result}");
            }
            else
                SetStatus(transcription.UsedBackup
                    ? "MAI backup text is ready. Use Copy last transcript in the v2t tray."
                    : "Text is ready. Use Copy last transcript from the v2t tray icon.", notify: true);
        }
        catch (Exception error) when (error is InvalidOperationException or Win32Exception or
            HttpRequestException or OperationCanceledException or InvalidDataException or IOException or TimeoutException)
        {
            if (!exiting && !cancelled)
            {
                var message = error is Win32Exception
                    ? "Could not finish microphone capture."
                    : error is OperationCanceledException ? "Transcription timed out." : error.Message;
                SetStatus(message, notify: true);
            }
        }
        finally
        {
            transcribing = false;
        }
    }

    private bool CanUseMaiBackup(DictationSettings current) =>
        current.EnableMaiBackup && openRouterKey is not null;

    private string? ApiKeyFor(DictationSettings current) =>
        current.Provider == SpeechProvider.OpenRouter ? openRouterKey : current.ApiKey;

    private SpeechResourceReason ReadResourceReason(
        DictationSettings current, nint foregroundWindow, bool observeGpu = true)
    {
        if (current.Provider != SpeechProvider.Local || !CanUseMaiBackup(current))
            return SpeechResourceReason.None;

        var snapshot = resourceProbe.Capture(foregroundWindow);
        if (!observeGpu || warmingWorkers > 0)
            snapshot = snapshot with { GpuUtilization = null };
        return resourceRouting.Update(snapshot);
    }

    private void MonitorResources()
    {
        if (exiting || IsDictating ||
            settings.Provider != SpeechProvider.Local || !CanUseMaiBackup(settings))
            return;

        var before = resourceRouting.Current;
        var reason = ReadResourceReason(settings, WindowsInput.CaptureFocus().Window);
        if (reason != SpeechResourceReason.None)
        {
            PauseLocalWorkers();
            if (reason != before)
                SetStatus($"MAI standby · {ResourceDescription(reason)}");
        }
        else if (retryInitialWarmup && pausedWorkers is not null)
        {
            // A model can cause enough pressure to pause itself again after loading.
            retryInitialWarmup = false;
            WarmInstalledModels();
        }
        else if (before != SpeechResourceReason.None)
        {
            SetStatus("Local will resume on the next dictation.");
        }
    }

    private void PauseLocalWorkers()
    {
        if (pausedWorkers is null || (pausedWorkers.IsCompletedSuccessfully && !pausedWorkers.Result))
            pausedWorkers = SuspendWorkersAsync();
    }

    private async Task<bool> SuspendWorkersAsync()
    {
        try
        {
            await Task.WhenAll(local.SuspendAsync(lifetime.Token), correctionEngine.SuspendAsync(lifetime.Token));
            return true;
        }
        catch (Exception error) when (error is OperationCanceledException or InvalidOperationException or Win32Exception)
        {
            if (!exiting && !IsDictating)
                SetStatus("MAI is ready. A local worker could not be released.");
            return false;
        }
    }

    private async Task ResumeLocalWorkersAsync()
    {
        if (pausedWorkers is not { } paused)
            return;

        await paused;
        if (ReferenceEquals(pausedWorkers, paused))
            pausedWorkers = null;
    }

    private static string ResourceDescription(SpeechResourceReason reason) => reason switch
    {
        SpeechResourceReason.Game => "game detected",
        SpeechResourceReason.LowRam => "low RAM",
        SpeechResourceReason.LowVram => "low VRAM",
        SpeechResourceReason.BusyGpu => "GPU busy",
        _ => "Local unavailable"
    };

    private async Task<SpeechFallbackResult> TranscribeDictationAsync(
        DictationSettings current, byte[] wave, SpeechResourceReason resourceReason)
    {
        var audioSeconds = (wave.Length - PcmWaveCapture.HeaderLength) / (double)PcmWaveCapture.BytesPerSecond;
        if (current.Provider != SpeechProvider.Local)
        {
            var model = current.Provider == SpeechProvider.OpenAI
                ? TranscriptionClient.OpenAiTranscriptionModel
                : TranscriptionClient.OpenRouterWhisperModel;
            var direct = await TrackSpeechAsync(current.Provider, model, audioSeconds,
                token => new TranscriptionClient(cloud).TranscribeWithUsageAsync(
                    current.Provider, wave, ApiKeyFor(current), current.Language, token), lifetime.Token);
            return new SpeechFallbackResult(direct, false);
        }

        Func<CancellationToken, Task<string>>? backup = null;
        if (current.EnableMaiBackup && openRouterKey is { } key)
        {
            backup = token =>
            {
                SetStatus(resourceReason == SpeechResourceReason.None
                    ? "Local could not transcribe. Trying MAI backup…"
                    : $"Using MAI · {ResourceDescription(resourceReason)}…");
                return TrackSpeechAsync(SpeechProvider.OpenRouter, TranscriptionClient.OpenRouterMaiModel,
                    audioSeconds, request => new TranscriptionClient(cloud)
                        .TranscribeMaiWithUsageAsync(wave, key, current.Language, request), token);
            };
        }

        async Task<string> TranscribeLocal(CancellationToken token)
        {
            retryInitialWarmup = false;
            await ResumeLocalWorkersAsync().WaitAsync(token);
            var client = await local.StartAsync(current.Language, token);
            return await TrackSpeechAsync(SpeechProvider.Local, local.ModelFileName ?? "Local Whisper",
                audioSeconds, request => client.TranscribeWithUsageAsync(
                    SpeechProvider.Local, wave, null, current.Language, request), token);
        }

        if (backup is null)
            return new SpeechFallbackResult(await TranscribeLocal(lifetime.Token), false);

        var localTimeout = TimeSpan.FromSeconds(Math.Min(60, 5 + audioSeconds / 4));
        return await SpeechFallback.RunAsync(TranscribeLocal, backup, localTimeout, lifetime.Token,
            preferBackup: resourceReason != SpeechResourceReason.None);
    }

    private async Task<string> TrackSpeechAsync(
        SpeechProvider provider, string model, double audioSeconds,
        Func<CancellationToken, Task<SpeechTranscription>> transcribe, CancellationToken cancellationToken)
    {
        var started = DateTimeOffset.Now;
        var stopwatch = Stopwatch.StartNew();
        SpeechTranscription? response = null;
        var outcome = "failed";
        try
        {
            response = await transcribe(cancellationToken);
            outcome = string.IsNullOrWhiteSpace(response.Text) ? "empty" : "ok";
            return response.Text;
        }
        catch (OperationCanceledException)
        {
            outcome = "canceled";
            throw;
        }
        finally
        {
            var (cost, costKind) = GetSpeechApiCost(provider, response, audioSeconds);
            RecordUsage(new UsageEntry(started, "speech", provider.ToString(), model, outcome, audioSeconds,
                stopwatch.Elapsed.TotalSeconds, response?.BilledSeconds, cost, costKind));
        }
    }

    private static (decimal? Cost, string Kind) GetSpeechApiCost(
        SpeechProvider provider, SpeechTranscription? response, double audioSeconds)
    {
        if (provider == SpeechProvider.Local)
            return (0m, "local");
        if (response?.ApiCostUsd is { } reported)
            return (reported, "reported");
        if (provider == SpeechProvider.OpenAI && response is not null)
            return (OpenAiTranscriptionUsdPerMinute * (decimal)audioSeconds / 60m, "estimated");
        return (null, "unknown");
    }

    private void RecordUsage(UsageEntry entry)
    {
        if (usageLog.TryAppend(entry))
        {
            usageLogUnavailable = false;
        }
        else if (!usageLogUnavailable && !exiting)
        {
            usageLogUnavailable = true;
            tray.ShowBalloonTip(2500, "v2t", "Could not write the local usage log.", ToolTipIcon.Warning);
        }
    }

    private async Task<string?> TryCorrectAsync(DictionaryMatch mapped)
    {
        SetStatus("Correcting text with local AI…");
        using var timeout = CancellationTokenSource.CreateLinkedTokenSource(lifetime.Token);
        timeout.CancelAfter(TimeSpan.FromSeconds(5));
        try
        {
            await ResumeLocalWorkersAsync().WaitAsync(timeout.Token);
            var client = await correctionEngine.StartAsync(timeout.Token);
            var started = DateTimeOffset.Now;
            var stopwatch = Stopwatch.StartNew();
            var outcome = "failed";
            try
            {
                var response = await client.CorrectAsync(mapped.Text, mapped.AppliedTerms, timeout.Token);
                outcome = response is null ? "fallback" : "ok";
                return response;
            }
            catch (OperationCanceledException)
            {
                outcome = "canceled";
                throw;
            }
            finally
            {
                RecordUsage(new UsageEntry(started, "correction", "Local", LocalCorrectionEngine.ModelFileName,
                    outcome, null, stopwatch.Elapsed.TotalSeconds, null, 0m, "local"));
            }
        }
        catch (Exception error) when (error is InvalidOperationException or Win32Exception or
            HttpRequestException or OperationCanceledException or IOException)
        {
            return null;
        }
    }

    private void ShowDictionary()
    {
        if (dictionaryForm is not null)
        {
            dictionaryForm.Activate();
            return;
        }

        using var dialog = new DictionaryForm(corrections, SaveCorrections,
            () => _ = InstallCorrectionAsync(), LocalCorrectionEngine.Installed, installingCorrection);
        dictionaryForm = dialog;
        try
        {
            if (form.Visible)
                dialog.ShowDialog(form);
            else
                dialog.ShowDialog();
        }
        finally
        {
            dictionaryForm = null;
        }
    }

    private string? SaveCorrections(CorrectionPreferences preferences)
    {
        try
        {
            CorrectionSettingsStore.Save(CorrectionFile, preferences);
            corrections = preferences;
            if (corrections.EnableAi && LocalCorrectionEngine.Installed)
                _ = WarmCorrectionAsync();
            return null;
        }
        catch (Exception error) when (error is IOException or InvalidDataException or UnauthorizedAccessException)
        {
            return "Could not save the dictionary for this Windows account.";
        }
    }

    private async Task InstallCorrectionAsync()
    {
        if (installingCorrection)
            return;
        installingCorrection = true;
        string? errorMessage = null;
        ReportCorrectionInstallation("Preparing the local model…");
        try
        {
            await LocalCorrectionEngine.InstallAsync(ReportCorrectionInstallation, lifetime.Token);
            if (!exiting && corrections.EnableAi)
                await WarmCorrectionAsync();
        }
        catch (Exception error) when (error is IOException or InvalidDataException or UnauthorizedAccessException or
            HttpRequestException or OperationCanceledException)
        {
            errorMessage = "Could not install the AI model. Check the connection and available disk space.";
        }
        finally
        {
            installingCorrection = false;
            if (!exiting)
                dictionaryForm?.UpdateInstallation(LocalCorrectionEngine.Installed, false, errorMessage);
        }
    }

    private void ReportCorrectionInstallation(string message)
    {
        if (!exiting)
            dictionaryForm?.UpdateInstallation(LocalCorrectionEngine.Installed, installingCorrection, message);
    }

    private async Task WarmCorrectionAsync()
    {
        if (ReadResourceReason(settings, WindowsInput.CaptureFocus().Window) != SpeechResourceReason.None)
        {
            PauseLocalWorkers();
            return;
        }

        warmingWorkers++;
        try
        {
            await ResumeLocalWorkersAsync();
            if (exiting || resourceRouting.Current != SpeechResourceReason.None)
                return;
            await correctionEngine.StartAsync(lifetime.Token);
        }
        catch (Exception error) when (error is Win32Exception or InvalidOperationException or OperationCanceledException or IOException)
        {
            if (!exiting && corrections.EnableAi && !IsDictating &&
                resourceRouting.Current == SpeechResourceReason.None)
                SetStatus("Local AI could not start. Dictionary replacements are available.");
        }
        finally
        {
            warmingWorkers--;
        }
    }

    private string? ApplySettings(DictationSettings updated)
    {
        var previous = settings;
        if (updated == previous)
            return null;
        if (IsDictating)
            return "Finish the current dictation before changing settings.";
        if (updated.OpenAiKey != previous.OpenAiKey && updated.OpenAiKey.Length != 0 &&
            !TranscriptionClient.HasBearerTokenCharacters(updated.OpenAiKey))
            return "The OpenAI key contains invalid characters.";

        var resolvedKey = OpenRouterKeySource.Resolve(updated.OpenRouterKey, AppContext.BaseDirectory);
        if (updated.OpenRouterKey != previous.OpenRouterKey &&
            !string.IsNullOrWhiteSpace(updated.OpenRouterKey) && resolvedKey is null)
            return "Enter a valid OpenRouter key or leave the field blank to use .env.";

        try
        {
            SettingsStore.Save(updated);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or
            System.Security.Cryptography.CryptographicException)
        {
            return "Could not save this change for this Windows account.";
        }

        bool routingChanged = previous.Provider != updated.Provider || previous.Language != updated.Language ||
            previous.EnableMaiBackup != updated.EnableMaiBackup || openRouterKey != resolvedKey;
        settings = updated;
        openRouterKey = resolvedKey;
        if (previous.Hotkey != updated.Hotkey)
            keyboard.SetShortcut(updated.Hotkey);
        if (routingChanged)
            resourceRouting.Reset();
        SetStatus($"Settings applied. Hold {ShortcutLabel} to speak.");
        if (routingChanged && updated.Provider == SpeechProvider.Local && LocalEngine.Available(updated.Language))
            _ = WarmLocalAsync();
        return null;
    }

    private async Task InstallLocalAsync(string? language)
    {
        if (installing)
            return;
        installing = true;
        form.SetInstalling(true);
        try
        {
            await LocalEngine.InstallAsync(language, message => SetStatus(message), lifetime.Token);
            if (!exiting && settings.Provider == SpeechProvider.Local && LocalEngine.Available(settings.Language) &&
                !IsDictating)
                await WarmLocalAsync();
            else if (!exiting && !IsDictating)
                SetStatus($"{LocalModel.PreferredFor(language).Name} installed.");
        }
        catch (Exception error) when (error is IOException or InvalidDataException or UnauthorizedAccessException or
            HttpRequestException or OperationCanceledException)
        {
            if (!exiting)
                SetStatus("Could not install the local model. Check the connection and available disk space.");
        }
        finally
        {
            installing = false;
            if (!exiting)
                form.SetInstalling(false);
        }
    }

    private async Task WarmLocalAsync()
    {
        var current = settings;
        if (ReadResourceReason(current, WindowsInput.CaptureFocus().Window) != SpeechResourceReason.None)
        {
            PauseLocalWorkers();
            return;
        }

        warmingWorkers++;
        if (!IsDictating)
            SetStatus("Loading local model…");
        try
        {
            await ResumeLocalWorkersAsync();
            if (exiting || !IsSelectedLocalLanguage(current.Language) || resourceRouting.Current != SpeechResourceReason.None)
                return;
            await local.StartAsync(current.Language, lifetime.Token);
            if (!exiting && IsSelectedLocalLanguage(current.Language) && !IsDictating)
            {
                var device = local.IsCuda ? " on NVIDIA GPU" : "";
                SetStatus($"{local.ModelName} ready{device}. Hold {ShortcutLabel} to speak.");
            }
        }
        catch (Exception error) when (error is Win32Exception or InvalidOperationException or OperationCanceledException)
        {
            if (!exiting && IsSelectedLocalLanguage(current.Language) && !IsDictating &&
                resourceRouting.Current == SpeechResourceReason.None)
                SetStatus(CanUseMaiBackup(settings)
                    ? "Local model could not start. MAI backup is available."
                    : $"Local model could not start. {error.Message}");
        }
        finally
        {
            warmingWorkers--;
        }
    }

    private bool IsSelectedLocalLanguage(string? language) =>
        settings.Provider == SpeechProvider.Local && settings.Language == language;

    private void CopyTranscript()
    {
        if (lastText is null)
            return;
        try
        {
            Clipboard.SetText(lastText);
            SetStatus("Transcript copied.");
        }
        catch (System.Runtime.InteropServices.ExternalException)
        {
            SetStatus("Clipboard is busy. Try copying again.", notify: true);
        }
    }

    private void SetStatus(string message, bool notify = false)
    {
        form.SetStatus(message);
        const string prefix = "v2t · ";
        var limit = 63 - prefix.Length;
        tray.Text = prefix + (message.Length <= limit ? message : $"{message[..(limit - 1)]}…");
        if (notify)
            tray.ShowBalloonTip(2500, "v2t", message, ToolTipIcon.Info);
    }

    private void ShowSettings()
    {
        form.Show();
        form.Activate();
    }

    protected override void ExitThreadCore()
    {
        exiting = true;
        SystemEvents.PowerModeChanged -= CleanupUsageOnResume;
        SystemEvents.TimeChanged -= CleanupUsageOnClockChange;
        usageCleanupTimer.Dispose();
        resourceTimer.Stop();
        resourceTimer.Dispose();
        resourceProbe.Dispose();
        SystemEvents.SessionSwitch -= ResetShortcut;
        lifetime.Cancel();
        keyboard.Dispose();
        CancelPendingRecording();
        recording?.Dispose();
        listeningCue.Dispose();
        local.Dispose();
        correctionEngine.Dispose();
        cloud.Dispose();
        tray.Visible = false;
        tray.ContextMenuStrip?.Dispose();
        tray.Dispose();
        trayIcon.Dispose();
        dictionaryForm?.Close();
        form.CloseForExit();
        lifetime.Dispose();
        base.ExitThreadCore();
    }
}
