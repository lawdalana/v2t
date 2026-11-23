using System.Buffers.Binary;
using System.Net;
using System.Net.Http.Headers;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace V2t.Core;

public enum SpeechProvider
{
    Local,
    OpenAI,
    OpenRouter
}

public sealed record SpeechTranscription(string Text, double? BilledSeconds, decimal? ApiCostUsd);

public sealed class UnsupportedLocalTranscriptionLanguageException()
    : InvalidOperationException("Whisper detected a language outside Thai and English. Try a longer phrase.")
{
}

public sealed class TranscriptionClient
{
    public const string MixedThaiEnglish = "th+en";
    public const string OpenAiTranscriptionModel = "gpt-transcribe";
    public const string OpenRouterWhisperModel = "openai/whisper-large-v3-turbo";
    public const string OpenRouterMaiModel = "microsoft/mai-transcribe-2";

    private const int MaxWaveBytes = 25_000_000;
    private const int MaxResponseBytes = 1024 * 1024;
    private const string MixedWhisperPrompt = "วันนี้มี meeting ตอนเช้า เดี๋ยวจะ review งานกับทีม แล้วคุยกันต่อ";
    private static readonly Uri DefaultLocalEndpoint = new("http://127.0.0.1:8178/inference");
    private static readonly Uri OpenAiEndpoint = new("https://api.openai.com/v1/audio/transcriptions");
    private static readonly Uri OpenRouterEndpoint = new("https://openrouter.ai/api/v1/audio/transcriptions");
    private static readonly JsonSerializerOptions OpenRouterJson = new()
    {
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull
    };

    private readonly HttpClient http;
    private readonly Uri localEndpoint;
    private readonly bool useMixedWhisperPrompt;

    public TranscriptionClient(HttpClient http, Uri? localEndpoint = null, bool useMixedWhisperPrompt = true)
    {
        ArgumentNullException.ThrowIfNull(http);
        this.http = http;
        this.localEndpoint = localEndpoint ?? DefaultLocalEndpoint;
        this.useMixedWhisperPrompt = useMixedWhisperPrompt;

        if (!this.localEndpoint.IsAbsoluteUri || this.localEndpoint.Scheme != Uri.UriSchemeHttp ||
            !this.localEndpoint.IsLoopback || this.localEndpoint.UserInfo.Length != 0)
        {
            throw new ArgumentException("The local transcription endpoint must use loopback HTTP.", nameof(localEndpoint));
        }
    }

    public async Task<string> TranscribeAsync(
        SpeechProvider provider, byte[] wave, string? apiKey, string? language, CancellationToken cancellationToken) =>
        (await TranscribeWithUsageAsync(provider, wave, apiKey, language, cancellationToken).ConfigureAwait(false)).Text;

    public async Task<SpeechTranscription> TranscribeWithUsageAsync(
        SpeechProvider provider, byte[] wave, string? apiKey, string? language, CancellationToken cancellationToken)
    {
        ValidateWave(wave);
        string? languageCode = NormalizeLanguage(language);
        bool mixedLocal = provider == SpeechProvider.Local && languageCode == MixedThaiEnglish;
        cancellationToken.ThrowIfCancellationRequested();
        if (mixedLocal && IsSilentPcm16(wave))
            return new SpeechTranscription("", null, null);

        using var request = CreateRequest(provider, wave, apiKey, languageCode);
        return await SendAsync(request, provider, mixedLocal, cancellationToken).ConfigureAwait(false);
    }

    public async Task<string> TranscribeMaiAsync(
        byte[] wave, string apiKey, string? language, CancellationToken cancellationToken) =>
        (await TranscribeMaiWithUsageAsync(wave, apiKey, language, cancellationToken).ConfigureAwait(false)).Text;

    public async Task<SpeechTranscription> TranscribeMaiWithUsageAsync(
        byte[] wave, string apiKey, string? language, CancellationToken cancellationToken)
    {
        ValidateWave(wave);
        string? languageCode = NormalizeLanguage(language);
        cancellationToken.ThrowIfCancellationRequested();
        var authorization = GetAuthorization(SpeechProvider.OpenRouter, apiKey);
        var payload = new
        {
            model = OpenRouterMaiModel,
            input_audio = new { data = wave, format = "wav" },
            language = languageCode == MixedThaiEnglish ? null : languageCode,
            provider = new { zdr = true, data_collection = "deny" }
        };
        using var request = new HttpRequestMessage(HttpMethod.Post, OpenRouterEndpoint);
        request.Headers.Authorization = authorization;
        var content = new ByteArrayContent(JsonSerializer.SerializeToUtf8Bytes(payload, OpenRouterJson));
        content.Headers.ContentType = new MediaTypeHeaderValue("application/json");
        request.Content = content;
        return await SendAsync(request, SpeechProvider.OpenRouter, mixedLocal: false, cancellationToken)
            .ConfigureAwait(false);
    }

    private async Task<SpeechTranscription> SendAsync(
        HttpRequestMessage request, SpeechProvider provider, bool mixedLocal, CancellationToken cancellationToken)
    {
        using var timeout = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        timeout.CancelAfter(http.Timeout);

        try
        {
            using var response = await http.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, timeout.Token)
                .ConfigureAwait(false);
            if (!response.IsSuccessStatusCode)
            {
                throw StatusError(provider, response.StatusCode);
            }

            await response.Content.LoadIntoBufferAsync(MaxResponseBytes, timeout.Token).ConfigureAwait(false);
            return await ReadTranscriptAsync(response.Content, provider, mixedLocal, timeout.Token).ConfigureAwait(false);
        }
        catch (OperationCanceledException) when (!cancellationToken.IsCancellationRequested)
        {
            throw new InvalidOperationException($"{provider} transcription timed out.");
        }
        catch (HttpRequestException)
        {
            throw new InvalidOperationException($"{provider} transcription could not complete the request.");
        }
        catch (IOException)
        {
            throw new InvalidOperationException($"{provider} transcription could not read the response.");
        }
    }

    private HttpRequestMessage CreateRequest(
        SpeechProvider provider, byte[] wave, string? apiKey, string? language)
    {
        if (provider == SpeechProvider.Local && http.DefaultRequestHeaders.Contains("Authorization"))
        {
            throw new InvalidOperationException("Local transcription requires a client without default authorization.");
        }

        var (endpoint, model) = provider switch
        {
            SpeechProvider.Local => (localEndpoint, null),
            SpeechProvider.OpenAI => (OpenAiEndpoint, OpenAiTranscriptionModel),
            SpeechProvider.OpenRouter => (OpenRouterEndpoint, OpenRouterWhisperModel),
            _ => throw new ArgumentOutOfRangeException(nameof(provider))
        };
        AuthenticationHeaderValue? authorization = model is null ? null : GetAuthorization(provider, apiKey);

        var request = new HttpRequestMessage(HttpMethod.Post, endpoint);
        request.Headers.Authorization = authorization;
        var form = new MultipartFormDataContent();
        request.Content = form;
        var audio = new ByteArrayContent(wave);
        audio.Headers.ContentType = new MediaTypeHeaderValue("audio/wav");
        form.Add(audio, "file", "dictation.wav");
        if (model is null)
        {
            bool mixed = language == MixedThaiEnglish;
            form.Add(new StringContent(language is null || mixed ? "auto" : language), "language");
            form.Add(new StringContent("false"), "translate");
            form.Add(new StringContent(mixed ? "verbose_json" : "json"), "response_format");
            if (mixed)
            {
                form.Add(new StringContent("true"), "no_language_probabilities");
                if (useMixedWhisperPrompt)
                    form.Add(new StringContent(MixedWhisperPrompt), "prompt");
            }
        }
        else
        {
            form.Add(new StringContent(model), "model");
            AddCloudLanguageHints(form, provider, language);
        }

        return request;
    }

    private static void AddCloudLanguageHints(MultipartFormDataContent form, SpeechProvider provider, string? language)
    {
        switch (provider, language)
        {
            case (SpeechProvider.OpenAI, MixedThaiEnglish):
                form.Add(new StringContent("th"), "languages[]");
                form.Add(new StringContent("en"), "languages[]");
                break;
            case (SpeechProvider.OpenAI, not null):
                form.Add(new StringContent(language), "languages[]");
                break;
            case (SpeechProvider.OpenRouter, not (null or MixedThaiEnglish)):
                form.Add(new StringContent(language), "language");
                break;
        }
    }

    private static void ValidateWave(byte[] wave)
    {
        ArgumentNullException.ThrowIfNull(wave);
        if (!HasWaveHeader(wave))
        {
            throw new ArgumentException("Expected a WAV recording with audio, up to 25 MB.", nameof(wave));
        }
    }

    private static bool HasWaveHeader(ReadOnlySpan<byte> wave) =>
        wave.Length is > 44 and <= MaxWaveBytes &&
        wave[..4].SequenceEqual("RIFF"u8) && wave.Slice(8, 4).SequenceEqual("WAVE"u8);

    public static bool IsSilentPcm16(ReadOnlySpan<byte> wave)
    {
        if (!HasWaveHeader(wave) || BinaryPrimitives.ReadUInt32LittleEndian(wave[4..]) != wave.Length - 8)
            return false;

        int blockAlignment = 0;
        for (int offset = 12; offset <= wave.Length - 8;)
        {
            var kind = wave.Slice(offset, 4);
            uint length = BinaryPrimitives.ReadUInt32LittleEndian(wave.Slice(offset + 4, 4));
            if (length > wave.Length - offset - 8)
                return false;

            int paddedLength = (int)length + (int)(length & 1);
            if (paddedLength > wave.Length - offset - 8)
                return false;

            var chunk = wave.Slice(offset + 8, (int)length);
            if (kind.SequenceEqual("fmt "u8))
                blockAlignment = Pcm16BlockAlignment(chunk);
            if (kind.SequenceEqual("data"u8))
                return blockAlignment != 0 && !chunk.IsEmpty && chunk.Length % blockAlignment == 0 &&
                    chunk.IndexOfAnyExcept((byte)0) == -1;

            offset += 8 + paddedLength;
        }
        return false;
    }

    private static int Pcm16BlockAlignment(ReadOnlySpan<byte> format)
    {
        if (format.Length < 16 || BinaryPrimitives.ReadUInt16LittleEndian(format) != 1 ||
            BinaryPrimitives.ReadUInt16LittleEndian(format[14..]) != 16)
            return 0;

        int channels = BinaryPrimitives.ReadUInt16LittleEndian(format[2..]);
        int blockAlignment = BinaryPrimitives.ReadUInt16LittleEndian(format[12..]);
        return blockAlignment == channels * sizeof(short) ? blockAlignment : 0;
    }

    private static string? NormalizeLanguage(string? language)
    {
        if (string.IsNullOrWhiteSpace(language) || string.Equals(language, "auto", StringComparison.OrdinalIgnoreCase))
        {
            return null;
        }

        if (string.Equals(language, MixedThaiEnglish, StringComparison.OrdinalIgnoreCase))
        {
            return MixedThaiEnglish;
        }

        if (language.Length != 2 || !char.IsAsciiLetter(language[0]) || !char.IsAsciiLetter(language[1]))
        {
            throw new ArgumentException("Use a two-letter language code or th+en for mixed Thai and English.", nameof(language));
        }

        return language.ToLowerInvariant();
    }

    private static AuthenticationHeaderValue GetAuthorization(SpeechProvider provider, string? apiKey)
    {
        string? key = apiKey?.Trim();
        if (string.IsNullOrEmpty(key))
        {
            throw new InvalidOperationException($"Enter an API key for {provider}.");
        }

        if (!HasBearerTokenCharacters(key))
        {
            throw new InvalidOperationException($"The {provider} API key contains invalid characters.");
        }

        return new AuthenticationHeaderValue("Bearer", key);
    }

    public static bool HasBearerTokenCharacters(string? token) =>
        !string.IsNullOrEmpty(token) &&
        token.All(c => char.IsAsciiLetterOrDigit(c) || c is '-' or '_' or '.' or '~' or '+' or '/' or '=');

    private static InvalidOperationException StatusError(SpeechProvider provider, HttpStatusCode status)
    {
        string detail = status switch
        {
            HttpStatusCode.Unauthorized or HttpStatusCode.Forbidden => "Check the API key and permissions.",
            HttpStatusCode.PaymentRequired => "Check the account balance.",
            HttpStatusCode.TooManyRequests => "The provider is rate limited. Try again shortly.",
            HttpStatusCode.ServiceUnavailable when provider == SpeechProvider.Local => "The local model may still be loading.",
            _ => "Try again or choose another provider."
        };
        return new InvalidOperationException($"{provider} transcription failed (HTTP {(int)status}). {detail}");
    }

    private static async Task<SpeechTranscription> ReadTranscriptAsync(
        HttpContent content, SpeechProvider provider, bool mixedLocal, CancellationToken cancellationToken)
    {
        try
        {
            using var stream = await content.ReadAsStreamAsync(cancellationToken).ConfigureAwait(false);
            using var document = await JsonDocument.ParseAsync(stream, cancellationToken: cancellationToken).ConfigureAwait(false);
            if (document.RootElement.ValueKind == JsonValueKind.Object &&
                document.RootElement.TryGetProperty("text", out var text) &&
                text.ValueKind == JsonValueKind.String)
            {
                var transcript = text.GetString()!.Trim();
                if (mixedLocal && transcript.Length > 0)
                    ValidateMixedWhisperLanguage(document.RootElement);
                return ReadUsage(document.RootElement, transcript);
            }
        }
        catch (JsonException)
        {
            // Provider payloads can contain sensitive details. Surface a fixed message below.
        }

        throw new InvalidOperationException($"{provider} returned an invalid transcription.");
    }

    private static SpeechTranscription ReadUsage(JsonElement response, string transcript)
    {
        if (!response.TryGetProperty("usage", out var usage) || usage.ValueKind != JsonValueKind.Object)
            return new SpeechTranscription(transcript, null, null);

        double? billedSeconds = usage.TryGetProperty("seconds", out var duration) &&
            duration.ValueKind == JsonValueKind.Number && duration.TryGetDouble(out var seconds) &&
            double.IsFinite(seconds) && seconds >= 0 ? seconds : null;
        decimal? apiCost = usage.TryGetProperty("cost", out var cost) && cost.ValueKind == JsonValueKind.Number &&
            cost.TryGetDecimal(out var dollars) && dollars >= 0 ? dollars : null;
        return new SpeechTranscription(transcript, billedSeconds, apiCost);
    }

    private static void ValidateMixedWhisperLanguage(JsonElement response)
    {
        if (!response.TryGetProperty("language", out var language) ||
            language.ValueKind != JsonValueKind.String)
        {
            throw new InvalidOperationException("Local Whisper did not report the detected language.");
        }

        if (language.GetString() is not ("thai" or "english" or "th" or "en"))
            throw new UnsupportedLocalTranscriptionLanguageException();
    }
}
