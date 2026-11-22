using System.Runtime.InteropServices;

namespace V2t.Windows;

public sealed class MicrophoneRecorder : IDisposable
{
    private const uint DefaultDevice = unchecked((uint)-1);
    private const uint EventCallback = 0x00050000;
    private const uint BufferDone = 1;
    private const int BufferCount = 4;
    private const int BufferBytes = 1600;
    private static readonly int HeaderSize = Marshal.SizeOf<WaveHeader>();
    private static readonly Task<byte[]> EmptyCapture = Task.FromResult(Array.Empty<byte>());

    private readonly AutoResetEvent signal = new(false);
    private readonly List<WaveBuffer> buffers = new(BufferCount);
    private Task<byte[]>? recording;
    private nint device;
    private int nextBuffer;
    private volatile bool stopRequested;

    public MicrophoneRecorder() : this(startImmediately: true) { }

    internal MicrophoneRecorder(bool startImmediately)
    {
        var format = new WaveFormat
        {
            Type = 1,
            Channels = 1,
            SamplesPerSecond = PcmWaveCapture.SampleRate,
            BytesPerSecond = PcmWaveCapture.BytesPerSecond,
            BlockAlign = 2,
            BitsPerSample = 16,
        };

        try
        {
            Check(waveInOpen(out device, DefaultDevice, ref format,
                signal.SafeWaitHandle.DangerousGetHandle(), 0, EventCallback), "open");
            for (int index = 0; index < BufferCount; index++)
            {
                var buffer = new WaveBuffer();
                buffers.Add(buffer);
                Check(waveInPrepareHeader(device, buffer.Header, HeaderSize), "prepare a buffer for");
                buffer.Prepared = true;
                Check(waveInAddBuffer(device, buffer.Header, HeaderSize), "queue a buffer for");
            }
            if (startImmediately)
                Start();
        }
        catch
        {
            Close();
            throw;
        }
    }

    internal void Start()
    {
        lock (signal)
        {
            if (stopRequested || recording is not null)
                throw new InvalidOperationException("The microphone recorder cannot be started again.");

            Check(waveInStart(device), "start");
            recording = Task.Factory.StartNew(Record, CancellationToken.None,
                TaskCreationOptions.LongRunning, TaskScheduler.Default);
        }
    }

    public Task<byte[]> StopAsync()
    {
        lock (signal)
        {
            if (!stopRequested)
            {
                stopRequested = true;
                if (recording is null)
                    Close();
                else
                    signal.Set();
            }
            return recording ?? EmptyCapture;
        }
    }

    private byte[] Record()
    {
        using var audio = new PcmWaveCapture();
        try
        {
            while (!stopRequested)
            {
                signal.WaitOne();
                if (!stopRequested && DrainBuffers(audio, requeue: true))
                    break;
            }

            // Stop returns the current partial buffer. Reset returns every queued buffer.
            Check(waveInStop(device), "stop");
            Check(waveInReset(device), "reset");
            DrainBuffers(audio, requeue: false);
            return audio.Finish();
        }
        finally
        {
            Close();
        }
    }

    private bool DrainBuffers(PcmWaveCapture audio, bool requeue)
    {
        for (int index = 0; index < BufferCount; index++)
        {
            WaveBuffer buffer = buffers[nextBuffer];
            WaveHeader header = Marshal.PtrToStructure<WaveHeader>(buffer.Header);
            if ((header.Flags & BufferDone) == 0)
                break;

            nextBuffer = (nextBuffer + 1) % BufferCount;
            if (audio.Append(buffer.Data.AsSpan(0, checked((int)header.BytesRecorded))))
                return true;
            if (requeue)
                Check(waveInAddBuffer(device, buffer.Header, HeaderSize), "requeue a buffer for");
        }
        return false;
    }

    private void Close()
    {
        if (device != 0)
        {
            waveInReset(device);
            foreach (WaveBuffer buffer in buffers)
            {
                if (buffer.Prepared)
                    waveInUnprepareHeader(device, buffer.Header, HeaderSize);
            }
            waveInClose(device);
        }
        foreach (WaveBuffer buffer in buffers)
            buffer.Dispose();

        lock (signal)
        {
            stopRequested = true;
            signal.Dispose();
        }
    }

    public void Dispose()
    {
        // StopAsync reports capture failures. Disposal only joins the capture thread.
        Task completion = StopAsync();
        completion.ConfigureAwait(ConfigureAwaitOptions.SuppressThrowing).GetAwaiter().GetResult();
    }

    private static void Check(uint result, string operation)
    {
        if (result != 0)
            throw new InvalidOperationException($"Could not {operation} the microphone (Windows audio error {result}).");
    }

    private sealed class WaveBuffer : IDisposable
    {
        private GCHandle pin;
        public byte[] Data { get; } = new byte[BufferBytes];
        public nint Header { get; }
        public bool Prepared { get; set; }

        public WaveBuffer()
        {
            pin = GCHandle.Alloc(Data, GCHandleType.Pinned);
            try
            {
                Header = Marshal.AllocHGlobal(HeaderSize);
                Marshal.StructureToPtr(new WaveHeader
                {
                    Data = pin.AddrOfPinnedObject(),
                    BufferLength = (uint)Data.Length,
                }, Header, false);
            }
            catch
            {
                Marshal.FreeHGlobal(Header);
                pin.Free();
                throw;
            }
        }

        public void Dispose()
        {
            Marshal.FreeHGlobal(Header);
            pin.Free();
        }
    }

    [StructLayout(LayoutKind.Sequential, Pack = 2)]
    private struct WaveFormat
    {
        public ushort Type, Channels;
        public uint SamplesPerSecond, BytesPerSecond;
        public ushort BlockAlign, BitsPerSample, ExtraSize;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct WaveHeader
    {
        public nint Data;
        public uint BufferLength, BytesRecorded;
        public nuint User;
        public uint Flags, Loops;
        public nint Next;
        public nuint Reserved;
    }

    [DllImport("winmm.dll")]
    private static extern uint waveInOpen(out nint device, uint deviceId, ref WaveFormat format,
        nint callback, nint instance, uint flags);

    [DllImport("winmm.dll")]
    private static extern uint waveInPrepareHeader(nint device, nint header, int size);

    [DllImport("winmm.dll")]
    private static extern uint waveInAddBuffer(nint device, nint header, int size);

    [DllImport("winmm.dll")]
    private static extern uint waveInStart(nint device);

    [DllImport("winmm.dll")]
    private static extern uint waveInStop(nint device);

    [DllImport("winmm.dll")]
    private static extern uint waveInReset(nint device);

    [DllImport("winmm.dll")]
    private static extern uint waveInUnprepareHeader(nint device, nint header, int size);

    [DllImport("winmm.dll")]
    private static extern uint waveInClose(nint device);
}

internal sealed class PcmWaveCapture : IDisposable
{
    internal const int SampleRate = 16000;
    internal const int BytesPerSecond = SampleRate * 2;
    internal const int HeaderLength = 44;
    private const int MaxBytes = BytesPerSecond * 5 * 60;
    private readonly MemoryStream stream = new() { Position = HeaderLength };

    internal bool Append(ReadOnlySpan<byte> samples)
    {
        int remaining = MaxBytes - ((int)stream.Position - HeaderLength);
        int bytes = Math.Min(samples.Length, remaining) & ~1;
        stream.Write(samples[..bytes]);
        return bytes == remaining;
    }

    internal byte[] Finish()
    {
        int sampleBytes = (int)stream.Position - HeaderLength;
        stream.Position = 0;
        using var writer = new BinaryWriter(stream, System.Text.Encoding.UTF8, leaveOpen: true);
        writer.Write("RIFF"u8);
        writer.Write(sampleBytes + HeaderLength - 8);
        writer.Write("WAVEfmt "u8);
        writer.Write(16);
        writer.Write((ushort)1);
        writer.Write((ushort)1);
        writer.Write(SampleRate);
        writer.Write(BytesPerSecond);
        writer.Write((ushort)2);
        writer.Write((ushort)16);
        writer.Write("data"u8);
        writer.Write(sampleBytes);
        return stream.ToArray();
    }

    public void Dispose() => stream.Dispose();
}
