using System.Buffers.Binary;

namespace V2t.Windows;

internal static class SpeechAudioGate
{
    private const int SamplesPerFrame = PcmWaveCapture.SampleRate / 50; // 20 ms
    private const int FrameBytes = SamplesPerFrame * sizeof(short);
    private const int RequiredConsecutiveFrames = 3;
    private const int MinimumRms = 48; // PCM16 amplitude, about −57 dBFS
    private const long MinimumCenteredEnergy = (long)MinimumRms * MinimumRms * SamplesPerFrame * SamplesPerFrame;

    // This energy check screens silence and short impulses. Sustained noise can still pass.
    internal static bool HasSpeech(byte[] wave)
    {
        ReadOnlySpan<byte> pcm = ReadRecorderSamples(wave);
        int consecutive = 0;
        for (int offset = 0; offset <= pcm.Length - FrameBytes; offset += FrameBytes)
        {
            consecutive = ExceedsFloor(pcm.Slice(offset, FrameBytes)) ? consecutive + 1 : 0;
            if (consecutive == RequiredConsecutiveFrames)
                return true;
        }
        return false;
    }

    private static bool ExceedsFloor(ReadOnlySpan<byte> frame)
    {
        long sum = 0;
        long sumSquares = 0;
        for (int offset = 0; offset < frame.Length; offset += sizeof(short))
        {
            int sample = BinaryPrimitives.ReadInt16LittleEndian(frame[offset..]);
            sum += sample;
            sumSquares += sample * sample;
        }

        // Compare variance without floating point so a steady microphone DC offset cannot trip the gate.
        return SamplesPerFrame * sumSquares - sum * sum >= MinimumCenteredEnergy;
    }

    private static ReadOnlySpan<byte> ReadRecorderSamples(ReadOnlySpan<byte> wave)
    {
        int sampleBytes = wave.Length - PcmWaveCapture.HeaderLength;
        if (sampleBytes < RequiredConsecutiveFrames * FrameBytes || sampleBytes % sizeof(short) != 0 ||
            !wave[..4].SequenceEqual("RIFF"u8) || !wave.Slice(8, 8).SequenceEqual("WAVEfmt "u8) ||
            BinaryPrimitives.ReadUInt32LittleEndian(wave[4..]) != wave.Length - 8 ||
            BinaryPrimitives.ReadUInt32LittleEndian(wave[16..]) != 16 ||
            BinaryPrimitives.ReadUInt16LittleEndian(wave[20..]) != 1 ||
            BinaryPrimitives.ReadUInt16LittleEndian(wave[22..]) != 1 ||
            BinaryPrimitives.ReadUInt32LittleEndian(wave[24..]) != PcmWaveCapture.SampleRate ||
            BinaryPrimitives.ReadUInt32LittleEndian(wave[28..]) != PcmWaveCapture.BytesPerSecond ||
            BinaryPrimitives.ReadUInt16LittleEndian(wave[32..]) != sizeof(short) ||
            BinaryPrimitives.ReadUInt16LittleEndian(wave[34..]) != 16 || !wave.Slice(36, 4).SequenceEqual("data"u8) ||
            BinaryPrimitives.ReadUInt32LittleEndian(wave[40..]) != sampleBytes)
            return [];

        return wave[PcmWaveCapture.HeaderLength..];
    }
}
