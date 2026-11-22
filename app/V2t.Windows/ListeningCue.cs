using System.Diagnostics;
using System.Runtime.InteropServices;

namespace V2t.Windows;

internal sealed class ListeningCue : IDisposable
{
    private const uint NoDefault = 0x0002;
    private const uint Memory = 0x0004;
    private static readonly TimeSpan SpeakerSettleTime = TimeSpan.FromMilliseconds(55);
    private readonly byte[]? clip;
    private readonly WaveOutListeningPlayer? player;
    private readonly object state = new();
    private readonly object output = new();
    private readonly ManualResetEvent stopping = new(false);
    private Task? warmup;
    private Task<bool> playback = Task.FromResult(true);
    private volatile bool disposed;

    internal ListeningCue()
    {
        using var wave = typeof(ListeningCue).Assembly.GetManifestResourceStream("V2t.Listening.wav");
        if (wave is null)
        {
            Trace.TraceWarning("v2t listening sound is missing.");
            return;
        }

        try
        {
            var bytes = new byte[checked((int)wave.Length)];
            wave.ReadExactly(bytes);
            clip = bytes;
            player = new WaveOutListeningPlayer(bytes);
        }
        catch (IOException error)
        {
            Trace.TraceWarning("v2t could not load its listening sound: {0}", error.Message);
        }
    }

    internal void WarmUp()
    {
        lock (state)
        {
            if (disposed || player is null || warmup is not null)
                return;
            warmup = Task.Run(() =>
            {
                lock (output)
                    if (!disposed)
                        player.Prepare();
            });
        }
    }

    internal Task<bool> PlayAsync()
    {
        lock (state)
        {
            if (disposed || clip is null)
                return Task.FromResult(true);
            return playback.IsCompleted ? playback = Task.Run(PlayAndSettleAsync) : playback;
        }
    }

    private async Task<bool> PlayAndSettleAsync()
    {
        (bool safe, bool settle) result;
        lock (output)
            result = disposed ? (true, false) : Play();
        if (result.settle)
            await Task.Delay(SpeakerSettleTime).ConfigureAwait(false);
        return result.safe;
    }

    private (bool Safe, bool Settle) Play()
    {
        if (player is not null)
        {
            // Retry only when WinMM rejected the write without placing a buffer in the queue.
            for (int attempt = 0; attempt < 2 && !disposed; attempt++)
            {
                if (!player.Prepare())
                {
                    if (player.HasOpenDevice)
                        return (!player.MayBePlaying, !player.MayBePlaying);
                    break;
                }

                var result = player.Play(stopping);
                if (result == WaveOutListeningPlayer.PlaybackResult.Completed)
                    return (true, true);
                bool closed = player.TryClose();
                if (result == WaveOutListeningPlayer.PlaybackResult.Uncertain || !closed)
                    return (!player.MayBePlaying, result == WaveOutListeningPlayer.PlaybackResult.Uncertain);
            }
        }

        if (disposed || clip is null)
            return (true, false);
        if (PlaySoundW(clip, 0, NoDefault | Memory))
            return (true, true);
        Trace.TraceWarning("Windows could not play the v2t listening sound.");
        return (true, false);
    }

    public void Dispose()
    {
        lock (state)
        {
            if (disposed)
                return;
            disposed = true;
            stopping.Set();
        }
        _ = Task.Run(() =>
        {
            lock (output)
            {
                if (player is not null && !player.TryClose())
                    Trace.TraceWarning("Windows could not release the v2t listening output.");
                stopping.Dispose();
            }
        });
    }

    [DllImport("winmm.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool PlaySoundW(byte[] sound, nint module, uint flags);
}
