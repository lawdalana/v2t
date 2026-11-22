using V2t.Core;

namespace V2t.Windows;

internal sealed record LocalModel(string Name, string FileName, string Url, string Sha256, bool UseMixedWhisperPrompt)
{
    internal static readonly LocalModel Whisper = new(
        "Whisper Large V3 Turbo",
        "ggml-large-v3-turbo-q5_0.bin",
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin?download=true",
        "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
        UseMixedWhisperPrompt: true);

    internal static readonly LocalModel Typhoon = new(
        "Typhoon Whisper Turbo",
        "ggml-typhoon-whisper-turbo-q5_0.bin",
        "https://huggingface.co/earyh774/typhoon-whisper-turbo-ggml/resolve/1881c5ba3e838e6ae22e11227d36736dde9c924e/ggml-typhoon-whisper-turbo-q5_0.bin?download=true",
        "2ffbb16719beda21f92f351ce66cef1e65cc2996b78556d1f77cbcc94ab88e67",
        UseMixedWhisperPrompt: false);

    internal static LocalModel PreferredFor(string? language) =>
        language is "th" or TranscriptionClient.MixedThaiEnglish ? Typhoon : Whisper;

    internal static LocalModel AvailableFor(string? language, Func<LocalModel, bool> installed)
    {
        var preferred = PreferredFor(language);
        return preferred == Typhoon && !installed(Typhoon) ? Whisper : preferred;
    }
}
