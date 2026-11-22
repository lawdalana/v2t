using System.Diagnostics;
using System.IO.Compression;
using System.Net;
using System.Net.Sockets;
using V2t.Core;

namespace V2t.Windows;

internal sealed class LocalEngine : IDisposable
{
    private const string EngineUrl = "https://github.com/ggml-org/whisper.cpp/releases/download/b5454/whisper-bin-x64.zip";
    private const string EngineHash = "6ba69e3482d7826214f90a6a9c84ca07782aec1e1d0c6a7c30c994fd5d816ccb";
    private static readonly string AssetDirectory = Path.Combine(SettingsStore.DirectoryPath, "Local");
    private static readonly string EngineDirectory = Path.Combine(AssetDirectory, "b5454");
    private static readonly string ServerFile = Path.Combine(EngineDirectory, "Release", "whisper-server.exe");
    private static readonly string CudaServerFile = Path.Combine(AssetDirectory, "b5454-cuda", "Release", "whisper-server.exe");
    private readonly HttpClient loopback = new(new SocketsHttpHandler
    {
        AllowAutoRedirect = false,
        UseProxy = false,
        ConnectTimeout = TimeSpan.FromSeconds(2)
    }) { Timeout = TimeSpan.FromSeconds(90) };

    private readonly LocalWorkerLifetime workerLifetime = new();
    private TranscriptionClient? loadedClient;
    private LocalModel? loadedModel;
    public bool IsCuda { get; private set; }
    public string? ModelName => loadedModel?.Name;
    internal string? ModelFileName => loadedModel?.FileName;

    public static bool PreferredInstalled(string? language) => Installed(LocalModel.PreferredFor(language));
    public static bool Available(string? language) => Installed(LocalModel.AvailableFor(language, Installed));

    private static bool Installed(LocalModel model) => File.Exists(ServerFile) && File.Exists(ModelPath(model));
    private static string ModelPath(LocalModel model) => Path.Combine(AssetDirectory, model.FileName);

    public static async Task InstallAsync(string? language, Action<string> report, CancellationToken cancellationToken)
    {
        Directory.CreateDirectory(AssetDirectory);
        using var download = new HttpClient { Timeout = TimeSpan.FromMinutes(10) };
        if (!File.Exists(ServerFile))
        {
            var archive = Path.Combine(AssetDirectory, "whisper-b5454.zip");
            var staging = EngineDirectory + "-" + Guid.NewGuid().ToString("N");
            report("Downloading the local engine…");
            try
            {
                await AssetDownload.SaveAsync(download, new Uri(EngineUrl), archive, EngineHash, null, cancellationToken);
                await Task.Run(() => ZipFile.ExtractToDirectory(archive, staging), cancellationToken);
                cancellationToken.ThrowIfCancellationRequested();
                Directory.Move(staging, EngineDirectory);
            }
            finally
            {
                File.Delete(archive);
                if (Directory.Exists(staging))
                    Directory.Delete(staging, recursive: true);
            }
        }

        var model = LocalModel.PreferredFor(language);
        var modelFile = ModelPath(model);
        if (!File.Exists(modelFile))
        {
            report($"Downloading {model.Name} (547 MiB)…");
            var progress = new Progress<int>(percent => report($"Downloading {model.Name}… {percent}%"));
            await AssetDownload.SaveAsync(download, new Uri(model.Url), modelFile, model.Sha256, progress, cancellationToken);
        }
    }

    public Task<TranscriptionClient> StartAsync(string? language, CancellationToken cancellationToken) =>
        workerLifetime.RunAsync(token => StartCachedAsync(language, token), cancellationToken);

    public Task SuspendAsync(CancellationToken cancellationToken) =>
        workerLifetime.SuspendAsync(ClearLoadedModel, cancellationToken);

    private async Task<TranscriptionClient> StartCachedAsync(string? language, CancellationToken cancellationToken)
    {
        var model = LocalModel.AvailableFor(language, Installed);
        if (!Installed(model))
            throw new InvalidOperationException("Open v2t settings and install the local model.");

        if (model == loadedModel && loadedClient is not null && workerLifetime.IsRunning)
            return loadedClient;

        ClearLoadedModel();
        await workerLifetime.StopAsync();
        try
        {
            var client = await StartPreferredEngineAsync(model, cancellationToken);
            cancellationToken.ThrowIfCancellationRequested();
            loadedModel = model;
            loadedClient = client;
            return client;
        }
        catch
        {
            await workerLifetime.StopAsync();
            throw;
        }
    }

    private void ClearLoadedModel()
    {
        loadedModel = null;
        loadedClient = null;
        IsCuda = false;
    }

    private async Task<TranscriptionClient> StartPreferredEngineAsync(LocalModel model, CancellationToken cancellationToken)
    {
        if (File.Exists(CudaServerFile))
        {
            try
            {
                var client = await StartProcessAsync(CudaServerFile, model, cancellationToken);
                cancellationToken.ThrowIfCancellationRequested();
                IsCuda = true;
                return client;
            }
            catch (Exception error) when (!cancellationToken.IsCancellationRequested &&
                error is InvalidOperationException or System.ComponentModel.Win32Exception)
            {
                await workerLifetime.StopAsync();
            }
        }

        return await StartProcessAsync(ServerFile, model, cancellationToken);
    }

    private async Task<TranscriptionClient> StartProcessAsync(string executable, LocalModel model, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        using var reservation = new TcpListener(IPAddress.Loopback, 0);
        reservation.Start();
        var port = ((IPEndPoint)reservation.LocalEndpoint).Port;
        var route = "/" + Guid.NewGuid().ToString("N");
        var start = new ProcessStartInfo(executable)
        {
            WorkingDirectory = Path.GetDirectoryName(executable)!,
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true
        };
        var threads = Math.Clamp(Environment.ProcessorCount / 2, 2, 8);
        foreach (var argument in new[]
        {
            "--host", "127.0.0.1", "--port", port.ToString(System.Globalization.CultureInfo.InvariantCulture),
            "--model", ModelPath(model), "--language", "auto", "--threads",
            threads.ToString(System.Globalization.CultureInfo.InvariantCulture), "--no-timestamps",
            "--request-path", route
        })
            start.ArgumentList.Add(argument);

        reservation.Stop();
        var worker = Process.Start(start) ?? throw new InvalidOperationException("Could not start local Whisper.");
        workerLifetime.Process = worker;
        worker.OutputDataReceived += (_, _) => { };
        worker.ErrorDataReceived += (_, _) => { };
        worker.BeginOutputReadLine();
        worker.BeginErrorReadLine();
        var baseUri = new Uri($"http://127.0.0.1:{port}{route}/");
        using var startup = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        startup.CancelAfter(TimeSpan.FromSeconds(35));
        try
        {
            while (true)
            {
                startup.Token.ThrowIfCancellationRequested();
                if (worker.HasExited)
                    break;
                try
                {
                    using var health = await loopback.GetAsync(new Uri(baseUri, "health"), startup.Token);
                    startup.Token.ThrowIfCancellationRequested();
                    if (health.IsSuccessStatusCode && !worker.HasExited)
                        return new TranscriptionClient(loopback, new Uri(baseUri, "inference"), model.UseMixedWhisperPrompt);
                }
                catch (HttpRequestException)
                {
                    // The server accepts connections after it initializes.
                }
                await Task.Delay(120, startup.Token);
            }

            throw new InvalidOperationException($"Local Whisper exited while starting ({worker.ExitCode}).");
        }
        catch (OperationCanceledException) when (!cancellationToken.IsCancellationRequested)
        {
            throw new InvalidOperationException("The local model timed out while loading.");
        }
    }

    public void Dispose()
    {
        ClearLoadedModel();
        try
        {
            workerLifetime.Dispose();
        }
        finally
        {
            loopback.Dispose();
        }
    }
}

// Engine operations start on the WinForms thread. The gate also orders continuations
// so a restarted worker cannot race an earlier startup or suspension.
internal sealed class LocalWorkerLifetime : IDisposable
{
    private readonly SemaphoreSlim gate = new(1);
    private CancellationTokenSource generation = new();
    private bool disposed;

    public Process? Process { get; set; }
    public bool IsRunning => Process is { HasExited: false };

    public async Task<T> RunAsync<T>(Func<CancellationToken, Task<T>> start, CancellationToken cancellationToken)
    {
        ObjectDisposedException.ThrowIf(disposed, this);
        using var linked = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken, generation.Token);
        await gate.WaitAsync(linked.Token);
        try
        {
            linked.Token.ThrowIfCancellationRequested();
            var result = await start(linked.Token);
            linked.Token.ThrowIfCancellationRequested();
            return result;
        }
        finally
        {
            gate.Release();
        }
    }

    public Task SuspendAsync(Action clear, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        if (disposed)
            return Task.CompletedTask;

        using var retired = generation;
        generation = new CancellationTokenSource();
        var suspension = SuspendWorkerAsync(clear);
        retired.Cancel();
        return suspension.WaitAsync(cancellationToken);
    }

    private async Task SuspendWorkerAsync(Action clear)
    {
        await gate.WaitAsync();
        try
        {
            if (disposed)
                return;
            clear();
            await StopAsync();
        }
        finally
        {
            gate.Release();
        }
    }

    public async Task StopAsync()
    {
        if (Process is not { } worker)
            return;

        Kill(worker);
        Process = null;
        using (worker)
            await worker.WaitForExitAsync();
    }

    public void Dispose()
    {
        if (disposed)
            return;
        disposed = true;
        generation.Cancel();
        generation.Dispose();

        if (Process is { } worker)
        {
            Process = null;
            using (worker)
                Kill(worker);
        }
    }

    private static void Kill(Process process)
    {
        try
        {
            if (!process.HasExited)
                process.Kill(entireProcessTree: true);
        }
        catch (InvalidOperationException)
        {
            // The process has already exited or was disposed together with the app.
        }
    }
}
