using System.Net;
using System.Net.Http.Headers;
using System.Text;
using System.Text.Json;
using V2t.Core;

namespace V2t.Tests;

internal static class TranscriptionTests
{
    private const string FakeKey = "fake-key-for-tests";
    private static readonly byte[] Wave = Convert.FromBase64String(
        "UklGRiYAAABXQVZFZm10IBAAAAABAAEAgD4AAAB9AAACABAAZGF0YQIAAAAqAA==");

    public static async Task RunAsync()
    {
        await SendsLocalWithoutLeakingCloudKey();
        await SendsToConfiguredLoopback();
        await SendsOpenAiWithExplicitLanguage();
        await SendsOpenAiWithAutomaticLanguage();
        await SendsOpenRouterWithAutomaticLanguage();
        await SendsOpenRouterWithExplicitLanguage();
        await SendsMaiBackupRequests();
        await ReadsUsageFromBothRequestPaths();
        await IgnoresInvalidOrMissingUsage();
        await SendsMixedLanguage();
        DetectsOnlyValidSilentPcm16();
        await RejectsInvalidInputBeforeSending();
        await RejectsDefaultAuthorizationForLocal();
        await ReportsCloudErrorsWithoutResponseContents();
        await ReportsTransportErrorsWithoutExceptionContents();
        await RejectsMalformedTranscripts();
        await PropagatesCancellation();
    }

    private static Task SendsLocalWithoutLeakingCloudKey() => VerifiesRequest(
        SpeechProvider.Local,
        "http://127.0.0.1:8178/inference",
        inputLanguage: null,
        expectedFields: [("language", "auto"), ("translate", "false"), ("response_format", "json")],
        expectedAuthorization: null);

    private static Task SendsToConfiguredLoopback()
    {
        const string endpoint = "http://127.0.0.1:4321/inference";
        return VerifiesRequest(SpeechProvider.Local, endpoint,
            inputLanguage: "th",
            expectedFields: [("language", "th"), ("translate", "false"), ("response_format", "json")],
            expectedAuthorization: null,
            localEndpoint: new Uri(endpoint));
    }

    private static Task SendsOpenAiWithExplicitLanguage() => VerifiesRequest(
        SpeechProvider.OpenAI,
        "https://api.openai.com/v1/audio/transcriptions",
        inputLanguage: "TH",
        expectedFields: [("model", "gpt-transcribe"), ("languages[]", "th")],
        expectedAuthorization: new AuthenticationHeaderValue("Bearer", FakeKey));

    private static Task SendsOpenAiWithAutomaticLanguage() => VerifiesRequest(
        SpeechProvider.OpenAI,
        "https://api.openai.com/v1/audio/transcriptions",
        inputLanguage: "auto",
        expectedFields: [("model", "gpt-transcribe")],
        expectedAuthorization: new AuthenticationHeaderValue("Bearer", FakeKey));

    private static Task SendsOpenRouterWithAutomaticLanguage() => VerifiesRequest(
        SpeechProvider.OpenRouter,
        "https://openrouter.ai/api/v1/audio/transcriptions",
        inputLanguage: "auto",
        expectedFields: [("model", "openai/whisper-large-v3-turbo")],
        expectedAuthorization: new AuthenticationHeaderValue("Bearer", FakeKey));

    private static Task SendsOpenRouterWithExplicitLanguage() => VerifiesRequest(
        SpeechProvider.OpenRouter,
        "https://openrouter.ai/api/v1/audio/transcriptions",
        inputLanguage: "en",
        expectedFields: [("model", "openai/whisper-large-v3-turbo"), ("language", "en")],
        expectedAuthorization: new AuthenticationHeaderValue("Bearer", FakeKey));

    private static async Task SendsMaiBackupRequests()
    {
        foreach (var (inputLanguage, expectedLanguage) in new (string?, string?)[] { ("th+en", null), ("auto", null), ("TH", "th") })
        {
            int requests = 0;
            using var http = new HttpClient(new Handler(async (request, token) =>
            {
                requests++;
                Check(request.Method == HttpMethod.Post, "Expected a MAI POST.");
                Check(request.RequestUri?.AbsoluteUri == "https://openrouter.ai/api/v1/audio/transcriptions", "Incorrect MAI URL.");
                Check(Equals(request.Headers.Authorization, new AuthenticationHeaderValue("Bearer", FakeKey)), "Expected MAI bearer authorization.");
                Check(request.Content?.Headers.ContentType?.MediaType == "application/json", "Expected a JSON upload for MAI.");
                using var document = JsonDocument.Parse(await request.Content!.ReadAsByteArrayAsync(token));
                var body = document.RootElement;
                Check(body.GetProperty("model").GetString() == "microsoft/mai-transcribe-2", "Wrong MAI model.");
                var audio = body.GetProperty("input_audio");
                Check(audio.EnumerateObject().Count() == 2, "Unexpected input_audio fields.");
                Check(audio.GetProperty("format").GetString() == "wav", "Expected WAV input.");
                Check(audio.GetProperty("data").GetBytesFromBase64().SequenceEqual(Wave), "Base64 WAV changed in transit.");
                if (expectedLanguage is null)
                {
                    Check(body.EnumerateObject().Count() == 2 && !body.TryGetProperty("language", out _),
                        "Mixed or auto mode must leave language detection to MAI.");
                }
                else
                {
                    Check(body.EnumerateObject().Count() == 3 && body.GetProperty("language").GetString() == expectedLanguage,
                        "Expected the explicit MAI language hint.");
                }
                return Json(HttpStatusCode.OK, """{"text":"  สวัสดี world  "}""");
            }));
            var client = new TranscriptionClient(http);
            Check(await client.TranscribeMaiAsync(Wave, FakeKey, inputLanguage, default) == "สวัสดี world",
                "Expected the trimmed MAI transcript.");
            Check(requests == 1, "MAI should make exactly one request.");
        }
    }

    private static async Task SendsMixedLanguage()
    {
        await VerifiesRequest(
            SpeechProvider.Local, "http://127.0.0.1:8178/inference", "th+en",
            [("language", "auto"), ("translate", "false"), ("response_format", "verbose_json"),
                ("no_language_probabilities", "true"), ("prompt", "วันนี้มี meeting ตอนเช้า เดี๋ยวจะ review งานกับทีม แล้วคุยกันต่อ")],
            null, responseJson: """{"text":"  สวัสดี world  ","language":"english"}""");
        await VerifiesRequest(
            SpeechProvider.Local, "http://127.0.0.1:8178/inference", "th+en",
            [("language", "auto"), ("translate", "false"), ("response_format", "verbose_json"),
                ("no_language_probabilities", "true")],
            null, responseJson: """{"text":"  สวัสดี world  ","language":"thai"}""", useMixedWhisperPrompt: false);
        var authorization = new AuthenticationHeaderValue("Bearer", FakeKey);
        await VerifiesRequest(
            SpeechProvider.OpenAI, "https://api.openai.com/v1/audio/transcriptions", "th+en",
            [("model", "gpt-transcribe"), ("languages[]", "th"), ("languages[]", "en")], authorization);
        await VerifiesRequest(
            SpeechProvider.OpenRouter, "https://openrouter.ai/api/v1/audio/transcriptions", "th+en",
            [("model", "openai/whisper-large-v3-turbo")], authorization);
        await RejectsOtherLanguagesInLocalMixedMode();
        await SkipsSilentMixedRecording();
    }

    private static async Task ReadsUsageFromBothRequestPaths()
    {
        using var http = new HttpClient(new Handler((_, _) => Task.FromResult(Json(HttpStatusCode.OK,
            """{"text":"  สวัสดี world  ","usage":{"seconds":8.25,"cost":0.000229166666666666666,"extra":true}}"""))));
        var client = new TranscriptionClient(http);
        var expected = new SpeechTranscription("สวัสดี world", 8.25, 0.000229166666666666666m);
        Check(await client.TranscribeWithUsageAsync(SpeechProvider.OpenRouter, Wave, FakeKey, null, default) == expected,
            "Expected billed seconds and exact decimal cost from direct transcription.");
        Check(await client.TranscribeMaiWithUsageAsync(Wave, FakeKey, "th+en", default) == expected,
            "Expected billed seconds and exact decimal cost from MAI.");
        Check(await client.TranscribeMaiAsync(Wave, FakeKey, "th+en", default) == expected.Text,
            "Existing text-only methods should ignore usage.");
    }

    private static async Task IgnoresInvalidOrMissingUsage()
    {
        foreach (var (body, seconds, cost) in new (string, double?, decimal?)[]
        {
            ("""{"text":"keep"}""", null, null),
            ("""{"text":"keep","usage":null}""", null, null),
            ("""{"text":"keep","usage":[]}""", null, null),
            ("""{"text":"keep","usage":{"seconds":"1.25","cost":0.0001}}""", null, 0.0001m),
            ("""{"text":"keep","usage":{"seconds":0,"cost":0}}""", 0, 0),
            ("""{"text":"keep","usage":{"seconds":-8,"cost":-0.1}}""", null, null),
            ("""{"text":"keep","usage":{"seconds":1e999,"cost":1e999}}""", null, null),
            ("""{"text":"keep","usage":{"seconds":2,"cost":{"amount":1}}}""", 2, null)
        })
        {
            using var http = new HttpClient(new Handler((_, _) => Task.FromResult(Json(HttpStatusCode.OK, body))));
            var client = new TranscriptionClient(http);
            var result = await client.TranscribeWithUsageAsync(SpeechProvider.OpenRouter, Wave, FakeKey, null, default);
            Check(result == new SpeechTranscription("keep", seconds, cost), "Malformed usage must preserve the transcript.");
        }
    }

    private static async Task SkipsSilentMixedRecording()
    {
        using var http = new HttpClient(new Handler((_, _) => throw new Exception("Silent audio reached the server.")));
        var client = new TranscriptionClient(http);
        var silent = (byte[])Wave.Clone();
        silent.AsSpan(44).Clear();
        Check(await client.TranscribeAsync(SpeechProvider.Local, silent, null, "th+en", default) == "",
            "Expected no text for silence.");

        using var wave = new MemoryStream();
        wave.Write(silent.AsSpan(0, 36));
        wave.Write("JUNK\u0002\u0000\u0000\u0000\u0000\u0000"u8);
        wave.Write(silent.AsSpan(36));
        var withMetadata = wave.ToArray();
        System.Buffers.Binary.BinaryPrimitives.WriteInt32LittleEndian(withMetadata.AsSpan(4, 4), withMetadata.Length - 8);
        Check(await client.TranscribeAsync(SpeechProvider.Local, withMetadata, null, "th+en", default) == "",
            "Expected no text for silence with a metadata chunk.");
        Check(TranscriptionClient.IsSilentPcm16(withMetadata), "A valid metadata chunk should be skipped.");
    }

    private static void DetectsOnlyValidSilentPcm16()
    {
        var silent = (byte[])Wave.Clone();
        silent.AsSpan(44).Clear();
        Check(TranscriptionClient.IsSilentPcm16(silent), "Expected true for a valid silent recording.");
        Check(!TranscriptionClient.IsSilentPcm16(Wave), "Audio that has nonzero samples cannot be silent.");
        Check(!TranscriptionClient.IsSilentPcm16(silent.AsSpan(0, 5)), "A truncated header is not PCM WAV.");

        var malformed = (byte[])silent.Clone();
        malformed[0] = 0;
        Check(!TranscriptionClient.IsSilentPcm16(malformed), "PCM detection requires a RIFF header.");
        malformed = (byte[])silent.Clone();
        malformed[8] = 0;
        Check(!TranscriptionClient.IsSilentPcm16(malformed), "PCM detection requires a WAVE container.");
        malformed = (byte[])silent.Clone();
        malformed[4] = 0;
        Check(!TranscriptionClient.IsSilentPcm16(malformed), "An invalid RIFF length cannot describe a silent WAV.");
        malformed = (byte[])silent.Clone();
        malformed[40] = 1;
        Check(!TranscriptionClient.IsSilentPcm16(malformed), "One byte is not a complete PCM16 sample.");
        malformed = (byte[])silent.Clone();
        malformed[20] = 3;
        Check(!TranscriptionClient.IsSilentPcm16(malformed), "Only PCM is eligible for the zero-sample check.");
    }

    private static async Task RejectsOtherLanguagesInLocalMixedMode()
    {
        var response = """{"text":"transcript","language":"vietnamese"}""";
        int requests = 0;
        using var http = new HttpClient(new Handler((_, _) =>
        {
            requests++;
            return Task.FromResult(Json(HttpStatusCode.OK, response));
        }));
        var client = new TranscriptionClient(http);
        var error = await ThrowsAsync<UnsupportedLocalTranscriptionLanguageException>(() =>
            client.TranscribeAsync(SpeechProvider.Local, Wave, null, "th+en", default));
        Check(error.Message.Contains("Thai and English"), "Expected a safe language mismatch message.");
        Check(requests == 1, "Language mismatch must not trigger a second request.");
        response = """{"text":"transcript"}""";
        var missingLanguage = await ThrowsAsync<InvalidOperationException>(() =>
            client.TranscribeAsync(SpeechProvider.Local, Wave, null, "th+en", default));
        Check(missingLanguage.GetType() == typeof(InvalidOperationException),
            "Missing language metadata should remain an operational error.");
        response = """{"text":"ไทยกับ English","language":"thai"}""";
        Check(await client.TranscribeAsync(SpeechProvider.Local, Wave, null, "th+en", default) == "ไทยกับ English",
            "Expected Thai and English in the same transcript.");
        response = """{"text":"","language":"unknown"}""";
        Check(await client.TranscribeAsync(SpeechProvider.Local, Wave, null, "th+en", default) == "",
            "An empty transcription should still report no speech.");
    }

    private static async Task VerifiesRequest(
        SpeechProvider provider, string url, string? inputLanguage,
        (string Name, string Value)[] expectedFields, AuthenticationHeaderValue? expectedAuthorization,
        Uri? localEndpoint = null, string responseJson = """{"text":"  สวัสดี world  "}""",
        bool useMixedWhisperPrompt = true)
    {
        int requestCount = 0;
        using var http = new HttpClient(new Handler(async (request, token) =>
        {
            requestCount++;
            Check(request.Method == HttpMethod.Post, "Expected POST.");
            Check(request.RequestUri?.AbsoluteUri == url, "Incorrect provider URL.");
            Check(Equals(request.Headers.Authorization, expectedAuthorization), "Authorization must be provider-specific.");
            var form = (MultipartFormDataContent?)request.Content;
            Check(form?.Headers.ContentType?.MediaType == "multipart/form-data", "Expected a multipart upload.");
            var parts = form!.ToArray();
            Check(parts.Length == expectedFields.Length + 1, "Incorrect multipart field count.");
            for (int index = 0; index < expectedFields.Length; index++)
            {
                var part = parts[index + 1];
                var (name, value) = expectedFields[index];
                Check(part.Headers.ContentDisposition!.Name!.Trim('"') == name, $"Missing multipart {name}.");
                Check(await part.ReadAsStringAsync(token) == value, $"Incorrect multipart {name}.");
            }

            var file = parts[0];
            Check(file.Headers.ContentDisposition!.Name!.Trim('"') == "file", "Missing file field.");
            Check(file.Headers.ContentDisposition!.FileName!.Trim('"').EndsWith(".wav"), "Missing WAV filename.");
            Check(file.Headers.ContentType?.MediaType == "audio/wav", "Expected WAV content type.");
            Check((await file.ReadAsByteArrayAsync(token)).SequenceEqual(Wave), "Audio changed in transit.");
            return Json(HttpStatusCode.OK, responseJson);
        }));
        var client = new TranscriptionClient(http, localEndpoint, useMixedWhisperPrompt);
        var result = await client.TranscribeAsync(provider, Wave, FakeKey, inputLanguage, default);
        Check(result == "สวัสดี world", "Expected a trimmed Unicode transcript.");
        Check(requestCount == 1, "A recording should result in exactly one request.");
    }

    private static async Task RejectsInvalidInputBeforeSending()
    {
        using var http = new HttpClient(new Handler((_, _) => throw new Exception("Unvalidated request sent.")));
        var client = new TranscriptionClient(http);
        await ThrowsAsync<ArgumentException>(async () =>
        {
            var external = new TranscriptionClient(http, new Uri("http://example.com/inference"));
            await external.TranscribeAsync(SpeechProvider.Local, Wave, null, "th", default);
        });
        await ThrowsAsync<InvalidOperationException>(() =>
            client.TranscribeAsync(SpeechProvider.OpenAI, Wave, " ", "th", default));
        await ThrowsAsync<InvalidOperationException>(() =>
            client.TranscribeAsync(SpeechProvider.OpenRouter, Wave, "value\r\nheader", "th", default));
        await ThrowsAsync<InvalidOperationException>(() =>
            client.TranscribeMaiAsync(Wave, " ", "th", default));
        await ThrowsAsync<ArgumentException>(() =>
            client.TranscribeAsync(SpeechProvider.Local, [1, 2, 3], null, "th", default));
        await ThrowsAsync<ArgumentException>(() =>
            client.TranscribeAsync(SpeechProvider.Local, Wave, null, "th\r\n", default));
    }

    private static async Task RejectsDefaultAuthorizationForLocal()
    {
        using var http = new HttpClient(new Handler((_, _) => throw new Exception("A default key was sent to Local.")));
        http.DefaultRequestHeaders.Authorization = new AuthenticationHeaderValue("Bearer", FakeKey);
        var client = new TranscriptionClient(http);
        var error = await ThrowsAsync<InvalidOperationException>(() =>
            client.TranscribeAsync(SpeechProvider.Local, Wave, null, null, default));
        Check(!error.ToString().Contains(FakeKey), "Error exposed an implicit key.");
    }

    private static async Task ReportsTransportErrorsWithoutExceptionContents()
    {
        using var http = new HttpClient(new Handler((_, _) => throw new HttpRequestException(FakeKey)));
        var client = new TranscriptionClient(http);
        var error = await ThrowsAsync<InvalidOperationException>(() =>
            client.TranscribeAsync(SpeechProvider.OpenAI, Wave, FakeKey, null, default));
        Check(error.Message.Contains("OpenAI"), "Expected affected provider.");
        Check(!error.ToString().Contains(FakeKey), "Error exposed a key through an exception.");
    }

    private static async Task ReportsCloudErrorsWithoutResponseContents()
    {
        using var http = new HttpClient(new Handler((_, _) => Task.FromResult(
            Json(HttpStatusCode.Unauthorized, $$$"""{"error":{"message":"Rejected key {{{FakeKey}}}"}}"""))));
        var client = new TranscriptionClient(http);
        var error = await ThrowsAsync<InvalidOperationException>(() =>
            client.TranscribeAsync(SpeechProvider.OpenAI, Wave, FakeKey, null, default));
        Check(error.Message.Contains("OpenAI") && error.Message.Contains("401"), "Expected provider and safe status.");
        Check(!error.ToString().Contains(FakeKey), "Provider error exposed a key.");
    }

    private static async Task RejectsMalformedTranscripts()
    {
        foreach (var body in new[] { "not JSON", "[]", """{"text":null}""", """{"text":123}""", """{"error":"offline"}""" })
        {
            using var http = new HttpClient(new Handler((_, _) => Task.FromResult(Json(HttpStatusCode.OK, body))));
            var client = new TranscriptionClient(http);
            await ThrowsAsync<InvalidOperationException>(() =>
                client.TranscribeAsync(SpeechProvider.Local, Wave, null, null, default));
        }

        using var emptyHttp = new HttpClient(new Handler((_, _) => Task.FromResult(
            Json(HttpStatusCode.OK, """{"text":"  "}"""))));
        var empty = new TranscriptionClient(emptyHttp);
        Check(await empty.TranscribeAsync(SpeechProvider.Local, Wave, null, null, default) == "",
            "Silence should return an empty transcript.");
    }

    private static async Task PropagatesCancellation()
    {
        using var http = new HttpClient(new Handler((_, token) =>
        {
            token.ThrowIfCancellationRequested();
            throw new Exception("Request token was not cancelled.");
        }));
        var client = new TranscriptionClient(http);
        using var source = new CancellationTokenSource();
        source.Cancel();
        await ThrowsAsync<OperationCanceledException>(() =>
            client.TranscribeAsync(SpeechProvider.Local, Wave, null, null, source.Token));
    }

    private static HttpResponseMessage Json(HttpStatusCode status, string body) =>
        new(status) { Content = new StringContent(body, Encoding.UTF8, "application/json") };

    private static async Task<T> ThrowsAsync<T>(Func<Task> action) where T : Exception
    {
        try
        {
            await action();
        }
        catch (T error)
        {
            return error;
        }

        throw new Exception($"Expected {typeof(T).Name}.");
    }

    private static void Check(bool condition, string message)
    {
        if (!condition)
        {
            throw new Exception(message);
        }
    }

    private sealed class Handler(Func<HttpRequestMessage, CancellationToken, Task<HttpResponseMessage>> send)
        : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken) =>
            send(request, cancellationToken);
    }
}
