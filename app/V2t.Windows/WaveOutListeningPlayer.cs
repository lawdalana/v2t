using System.Buffers.Binary;
using System.Diagnostics;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace V2t.Windows;

internal sealed class WaveOutListeningPlayer(byte[] wave)
{
    private const uint DefaultDevice = uint.MaxValue;
    private const uint MapperPreferredGet = 0x2015;
    private const uint EventCallback = 0x00050000;
    private const uint BufferDone = 0x0001;
    private const uint BufferInQueue = 0x0010;
    private const uint InvalidHandle = 5;
    private static readonly uint HeaderSize = (uint)Marshal.SizeOf<WaveHeader>();
    private static readonly int FlagsOffset = (int)Marshal.OffsetOf<WaveHeader>(nameof(WaveHeader.Flags));
    private static readonly TimeSpan PlaybackTimeout = TimeSpan.FromSeconds(1);
    private nint device;
    private nint header;
    private uint? preferredAtOpen;
    private AutoResetEvent? completed;
    private SafeWaitHandle? callbackHandle;
    private GCHandle samples;
    private bool retiring;
    internal bool IsPrepared { get; private set; }
    internal bool HasOpenDevice => device != 0;
    internal bool MayBePlaying { get; private set; }

    internal bool Prepare()
    {
        uint? preferred = waveOutMessage(-1, MapperPreferredGet, out uint current, out _) == 0 ? current : null;
        if (!retiring && IsPrepared && (preferred is null || preferredAtOpen == preferred))
            return true;
        if (!TryClose() || !TryReadFormat(wave, out var format))
            return false;

        completed = new AutoResetEvent(false);
        var handle = completed.SafeWaitHandle;
        bool retained = false;
        try
        {
            handle.DangerousAddRef(ref retained);
            callbackHandle = handle;
            if (waveOutOpen(out device, DefaultDevice, ref format, handle.DangerousGetHandle(), 0, EventCallback) != 0)
            {
                device = 0;
                return false;
            }

            samples = GCHandle.Alloc(wave, GCHandleType.Pinned);
            header = Marshal.AllocHGlobal((int)HeaderSize);
            Marshal.StructureToPtr(new WaveHeader
            {
                Data = samples.AddrOfPinnedObject() + 44,
                Length = (uint)(wave.Length - 44)
            }, header, false);
            IsPrepared = waveOutPrepareHeader(device, header, HeaderSize) == 0;
            preferredAtOpen = preferred;
            return IsPrepared;
        }
        finally
        {
            if (!IsPrepared)
            {
                if (!retained)
                    callbackHandle = null;
                TryClose();
            }
        }
    }

    internal PlaybackResult Play(WaitHandle stopping)
    {
        if (retiring || !IsPrepared || completed is not { } signal || stopping.WaitOne(0))
            return PlaybackResult.NotQueued;

        signal.Reset();
        uint result = waveOutWrite(device, header, HeaderSize);
        if (result != 0)
        {
            // These errors reject the write. On an unknown result, assume it might have played.
            MayBePlaying = (result is not (2 or InvalidHandle or 6 or 7 or 34)) || (Flags() & BufferInQueue) != 0;
            return MayBePlaying ? PlaybackResult.Uncertain : PlaybackResult.NotQueued;
        }

        MayBePlaying = true;
        WaitHandle[] signals = [stopping, signal];
        var started = Stopwatch.StartNew();
        while ((Flags() & BufferDone) == 0)
        {
            var remaining = PlaybackTimeout - started.Elapsed;
            if (remaining <= TimeSpan.Zero || WaitHandle.WaitAny(signals, remaining) != 1)
                return PlaybackResult.Uncertain;
        }
        MayBePlaying = false;
        return PlaybackResult.Completed;
    }

    internal bool TryClose()
    {
        if (device != 0)
        {
            retiring = true;
            if (IsPrepared)
            {
                if (waveOutReset(device) is 0 or InvalidHandle)
                    MayBePlaying = false;
                uint result = waveOutUnprepareHeader(device, header, HeaderSize);
                if (result is not (0 or InvalidHandle))
                    return false;
                IsPrepared = false;
                MayBePlaying = false;
            }
            if (waveOutClose(device) is not (0 or InvalidHandle))
                return false;
            device = 0;
        }

        // The driver must release its buffer and callback handle before either can be freed.
        if (header != 0)
        {
            Marshal.FreeHGlobal(header);
            header = 0;
        }
        if (samples.IsAllocated)
            samples.Free();
        callbackHandle?.DangerousRelease();
        callbackHandle = null;
        completed?.Dispose();
        completed = null;
        retiring = false;
        MayBePlaying = false;
        return true;
    }

    private uint Flags() => (uint)Marshal.ReadInt32(header, FlagsOffset);

    private static bool TryReadFormat(ReadOnlySpan<byte> wav, out WaveFormat format)
    {
        format = default;
        if (wav.Length <= 44 || !wav[..4].SequenceEqual("RIFF"u8) || !wav.Slice(8, 8).SequenceEqual("WAVEfmt "u8) ||
            BinaryPrimitives.ReadUInt32LittleEndian(wav[16..]) != 16 ||
            !wav.Slice(36, 4).SequenceEqual("data"u8) || BinaryPrimitives.ReadUInt32LittleEndian(wav[40..]) != wav.Length - 44)
            return false;

        format = new WaveFormat
        {
            Encoding = BinaryPrimitives.ReadUInt16LittleEndian(wav[20..]),
            Channels = BinaryPrimitives.ReadUInt16LittleEndian(wav[22..]),
            SamplesPerSecond = BinaryPrimitives.ReadUInt32LittleEndian(wav[24..]),
            BytesPerSecond = BinaryPrimitives.ReadUInt32LittleEndian(wav[28..]),
            BlockAlign = BinaryPrimitives.ReadUInt16LittleEndian(wav[32..]),
            BitsPerSample = BinaryPrimitives.ReadUInt16LittleEndian(wav[34..])
        };
        return format.Encoding == 1 && format.Channels is 1 or 2 && format.BitsPerSample == 16 &&
            format.SamplesPerSecond > 0 && format.BlockAlign == format.Channels * sizeof(short) &&
            format.BytesPerSecond == (ulong)format.SamplesPerSecond * format.BlockAlign &&
            (wav.Length - 44) % format.BlockAlign == 0;
    }

    internal enum PlaybackResult { NotQueued, Completed, Uncertain }

    [StructLayout(LayoutKind.Sequential, Pack = 2)]
    private struct WaveFormat
    {
        public ushort Encoding, Channels;
        public uint SamplesPerSecond, BytesPerSecond;
        public ushort BlockAlign, BitsPerSample, ExtraSize;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct WaveHeader
    {
        public nint Data;
        public uint Length, Recorded;
        public nuint User;
        public uint Flags, Loops;
        public nint Next;
        public nuint Reserved;
    }

    [DllImport("winmm.dll", ExactSpelling = true)] private static extern uint waveOutMessage(nint device, uint message, out uint preferred, out uint flags);
    [DllImport("winmm.dll", ExactSpelling = true)] private static extern uint waveOutOpen(out nint device, uint id, ref WaveFormat format, nint callback, nint instance, uint flags);
    [DllImport("winmm.dll", ExactSpelling = true)] private static extern uint waveOutPrepareHeader(nint device, nint header, uint size);
    [DllImport("winmm.dll", ExactSpelling = true)] private static extern uint waveOutWrite(nint device, nint header, uint size);
    [DllImport("winmm.dll", ExactSpelling = true)] private static extern uint waveOutReset(nint device);
    [DllImport("winmm.dll", ExactSpelling = true)] private static extern uint waveOutUnprepareHeader(nint device, nint header, uint size);
    [DllImport("winmm.dll", ExactSpelling = true)] private static extern uint waveOutClose(nint device);
}
