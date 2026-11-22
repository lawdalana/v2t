using System.Buffers.Binary;
using V2t.Windows;

namespace V2t.Tests;

internal static class SpeechAudioGateTests
{
    private const int Rate = PcmWaveCapture.SampleRate;

    internal static void Run()
    {
        RejectsSilenceAndDcOffset();
        RejectsLowRoomNoise();
        RejectsIsolatedImpulseAndShortBurst();
        RejectsInterruptedNoiseFrames();
        PreservesQuietShortSustainedAudio();
        RejectsInvalidOrUnsupportedWave();
        Console.WriteLine("SpeechAudioGate: 6 tests passed");
    }

    private static void RejectsSilenceAndDcOffset()
    {
        AssertEx.True(!SpeechAudioGate.HasSpeech(BuildWave(700, _ => 0)), "Exact silence should not be sent.");
        AssertEx.True(!SpeechAudioGate.HasSpeech(BuildWave(700, _ => 1500)), "A fixed DC offset is not speech.");
    }

    private static void RejectsLowRoomNoise()
    {
        var noise = new Random(81);
        var wave = BuildWave(1600, sample => (short)(150 + noise.Next(-30, 31) + Tone(sample, 12, 120)));
        AssertEx.True(!SpeechAudioGate.HasSpeech(wave), "Low room tone should not be sent.");
    }

    private static void RejectsIsolatedImpulseAndShortBurst()
    {
        AssertEx.True(!SpeechAudioGate.HasSpeech(BuildWave(400, sample => sample == 1700 ? short.MinValue : (short)0)),
            "A loud single-sample spike is not a sustained sound.");
        var burst = BuildWave(400, sample => sample is >= 1490 and < 1800 ? Tone(sample, 9000, 880) : (short)0);
        AssertEx.True(!SpeechAudioGate.HasSpeech(burst), "A short transient crossing frames should not be sent.");
    }

    private static void RejectsInterruptedNoiseFrames()
    {
        var wave = BuildWave(800, sample => sample / (Rate / 50) % 2 == 0 ? Tone(sample, 300, 420) : (short)0);
        AssertEx.True(!SpeechAudioGate.HasSpeech(wave), "Separated transients must not accumulate into speech.");
    }

    private static void PreservesQuietShortSustainedAudio()
    {
        var noise = new Random(17);
        var wave = BuildWave(180, sample =>
        {
            var voice = sample is >= 573 and < 2173 ? Tone(sample, 78, 173) : 0;
            return (short)(120 + voice + noise.Next(-8, 9));
        });
        AssertEx.True(SpeechAudioGate.HasSpeech(wave), "A quiet 100 ms voiced segment should pass.");
    }

    private static void RejectsInvalidOrUnsupportedWave()
    {
        AssertEx.True(!SpeechAudioGate.HasSpeech([]));
        AssertEx.True(!SpeechAudioGate.HasSpeech(BuildWave(50, sample => Tone(sample, 1000, 180))));
        var valid = BuildWave(200, sample => Tone(sample, 1000, 180));
        AssertEx.True(SpeechAudioGate.HasSpeech(valid));
        AssertEx.True(!SpeechAudioGate.HasSpeech(valid[..^1]), "A truncated data chunk should not pass.");
        valid[22] = 2;
        AssertEx.True(!SpeechAudioGate.HasSpeech(valid), "The gate only accepts the recorder's mono format.");
    }

    private static short Tone(int sample, double peak, int frequency) =>
        (short)Math.Round(peak * Math.Sin(2 * Math.PI * frequency * sample / Rate));

    private static byte[] BuildWave(int milliseconds, Func<int, short> valueAt)
    {
        int samples = milliseconds * Rate / 1000;
        var bytes = new byte[samples * sizeof(short)];
        for (int sample = 0; sample < samples; sample++)
            BinaryPrimitives.WriteInt16LittleEndian(bytes.AsSpan(sample * sizeof(short)), valueAt(sample));
        using var capture = new PcmWaveCapture();
        capture.Append(bytes);
        return capture.Finish();
    }
}
