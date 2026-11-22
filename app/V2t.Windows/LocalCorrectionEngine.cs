using System.ComponentModel;
using System.Diagnostics;
using System.IO.Compression;
using System.Net;
using System.Net.Sockets;
using System.Security.Cryptography;
using V2t.Core;

namespace V2t.Windows;

internal sealed class LocalCorrectionEngine : IDisposable
{
    private const string EngineUrl = "https://github.com/ggml-org/llama.cpp/releases/download/b12633/llama-b12633-bin-win-vulkan-x64.zip";
    private const string EngineHash = "720d59998a51423a1c50c99326a5d5358b80e89d75f8fdafc50a26479d1bad68";
    private const string ModelUrl = "https://huggingface.co/typhoon-ai/typhoon2.5-qwen3-4b-gguf/resolve/386874d02430a544367cb0878687f8983751bbd2/typhoon2.5-qwen3-4b-q4_k_m.gguf?download=true";
    private const string ModelHash = "b365302922688f3a3c9ac8e3c00ab97a152cac0cdbf4eb5a734ecb483ae3e511";
    private static readonly string AssetDirectory = Path.Combine(SettingsStore.DirectoryPath, "Correction");
    private static readonly string EngineDirectory = Path.Combine(AssetDirectory, "b12633");
    private static readonly string ServerFile = Path.Combine(EngineDirectory, "llama-server.exe");
    internal const string ModelFileName = "typhoon2.5-qwen3-4b-q4_k_m.gguf";
    private static readonly string ModelFile = Path.Combine(AssetDirectory, ModelFileName);
    private readonly HttpClient loopback = new(new SocketsHttpHandler
    {
        AllowAutoRedirect = false, UseProxy = false, ConnectTimeout = TimeSpan.FromSeconds(1)
    }) { Timeout = TimeSpan.FromSeconds(8) };
    private readonly LocalWorkerLifetime workerLifetime = new();
    private DictationCorrectionClient? loadedClient;

    public static bool Installed => File.Exists(ServerFile) && File.Exists(ModelFile);

    public static async Task InstallAsync(Action<string> report, CancellationToken cancellationToken)
    {
        Directory.CreateDirectory(AssetDirectory);
        using var download = new HttpClient { Timeout = TimeSpan.FromMinutes(15) };
        if (!File.Exists(ServerFile))
        {
            var archive = Path.Combine(AssetDirectory, "llama-b12633.zip");
            var staging = EngineDirectory + "-" + Guid.NewGuid().ToString("N");
            report("Downloading the AI engine…");
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

        if (!File.Exists(ModelFile))
        {
            report("Downloading the Typhoon correction model (2.3 GiB)…");
            var progress = new Progress<int>(percent => report($"Downloading Typhoon… {percent}%"));
            await AssetDownload.SaveAsync(download, new Uri(ModelUrl), ModelFile, ModelHash, progress, cancellationToken);
        }
    }

    public Task<DictationCorrectionClient> StartAsync(CancellationToken cancellationToken = default) =>
        workerLifetime.RunAsync(StartCachedAsync, cancellationToken);

    public Task SuspendAsync(CancellationToken cancellationToken) =>
        workerLifetime.SuspendAsync(() => loadedClient = null, cancellationToken);

    private async Task<DictationCorrectionClient> StartCachedAsync(CancellationToken cancellationToken)
    {
        if (!Installed)
            throw new InvalidOperationException("Install the local AI model in Dictionary & AI.");

        if (loadedClient is not null && workerLifetime.IsRunning)
            return loadedClient;

        loadedClient = null;
        await workerLifetime.StopAsync();
        try
        {
            var client = await StartWithFallbackAsync(cancellationToken);
            cancellationToken.ThrowIfCancellationRequested();
            loadedClient = client;
            return client;
        }
        catch
        {
            await workerLifetime.StopAsync();
            throw;
        }
    }

    private async Task<DictationCorrectionClient> StartWithFallbackAsync(CancellationToken cancellationToken)
    {
        if (File.Exists(Path.Combine(Environment.SystemDirectory, "vulkan-1.dll")))
        {
            try
            {
                return await StartProcessAsync(useGpu: true, cancellationToken);
            }
            catch (Exception error) when (!cancellationToken.IsCancellationRequested &&
                (error is Win32Exception or InvalidOperationException))
            {
                // A machine without a usable Vulkan device can still run the CPU backend.
                await workerLifetime.StopAsync();
            }
        }

        return await StartProcessAsync(useGpu: false, cancellationToken);
    }

    private async Task<DictationCorrectionClient> StartProcessAsync(bool useGpu, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        using var reservation = new TcpListener(IPAddress.Loopback, 0);
        reservation.Start();
        var port = ((IPEndPoint)reservation.LocalEndpoint).Port;
        var apiKey = Convert.ToHexString(RandomNumberGenerator.GetBytes(32));

        var start = new ProcessStartInfo(ServerFile)
        {
            WorkingDirectory = EngineDirectory,
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true
        };
        foreach (var argument in new[]
        {
            "--host", "127.0.0.1", "--port", port.ToString(System.Globalization.CultureInfo.InvariantCulture),
            "--model", ModelFile, "--alias", "v2t-correction", "--api-key", apiKey, "--no-webui",
            "--jinja", "--reasoning", "off", "--ctx-size", "4096", "--parallel", "1",
            "--threads", "6", "--n-gpu-layers", useGpu ? "99" : "0", "--log-disable"
        })
            start.ArgumentList.Add(argument);
        if (!useGpu)
        {
            start.ArgumentList.Add("--device");
            start.ArgumentList.Add("none");
        }

        reservation.Stop();
        var worker = Process.Start(start) ?? throw new InvalidOperationException("Could not start the local AI engine.");
        workerLifetime.Process = worker;
        worker.OutputDataReceived += (_, _) => { };
        worker.ErrorDataReceived += (_, _) => { };
        worker.BeginOutputReadLine();
        worker.BeginErrorReadLine();

        var address = new Uri($"http://127.0.0.1:{port}/");
        using var readiness = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        readiness.CancelAfter(TimeSpan.FromSeconds(30));
        try
        {
            while (true)
            {
                readiness.Token.ThrowIfCancellationRequested();
                if (worker.HasExited)
                    break;
                try
                {
                    using var health = await loopback.GetAsync(new Uri(address, "health"), readiness.Token);
                    readiness.Token.ThrowIfCancellationRequested();
                    if (health.IsSuccessStatusCode && !worker.HasExited)
                    {
                        var client = new DictationCorrectionClient(loopback, new Uri(address, "v1/chat/completions"), apiKey);
                        // Prime the local inference backend before the first recording needs it.
                        await client.CorrectAsync("ทดสอบ OpenRouter.", ["OpenRouter"], cancellationToken);
                        return client;
                    }
                }
                catch (HttpRequestException)
                {
                    // The engine starts listening after initialization.
                }
                await Task.Delay(120, readiness.Token);
            }
            throw new InvalidOperationException("The local AI engine exited while loading.");
        }
        catch (OperationCanceledException) when (!cancellationToken.IsCancellationRequested)
        {
            throw new InvalidOperationException("The local AI model took too long to load.");
        }
    }

    public void Dispose()
    {
        loadedClient = null;
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
