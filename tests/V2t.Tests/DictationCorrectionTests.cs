using System.Net;
using System.Text;
using System.Text.Json;
using V2t.Core;

namespace V2t.Tests;

internal static class DictationCorrectionTests
{
    private static readonly Uri Endpoint = new("http://127.0.0.1:8179/v1/chat/completions");
    private const string LocalKey = "local-opaque-test-key";
    private const string Text = "สวัสดี OpenAI เจอกันวันุพธ";

    internal static async Task RunAsync()
    {
        await SendsBoundedStructuredRequest();
        await FallsBackOnInvalidResults();
        await SkipsInvalidInput();
        RejectsRemoteEndpoint();
    }

    private static async Task SendsBoundedStructuredRequest()
    {
        int requests = 0;
        using var http = new HttpClient(new Stub(async (request, token) =>
        {
            requests++;
            AssertEx.Equal(Endpoint, request.RequestUri);
            AssertEx.Equal(HttpMethod.Post, request.Method);
            AssertEx.Equal("Bearer", request.Headers.Authorization?.Scheme);
            AssertEx.Equal(LocalKey, request.Headers.Authorization?.Parameter);
            AssertEx.Equal("application/json", request.Content?.Headers.ContentType?.MediaType);
            using var body = JsonDocument.Parse(await request.Content!.ReadAsStringAsync(token));
            JsonElement root = body.RootElement;
            AssertEx.Equal("v2t-correction", root.GetProperty("model").GetString());
            AssertEx.Equal(0, root.GetProperty("temperature").GetInt32());
            AssertEx.True(!root.GetProperty("stream").GetBoolean());
            AssertEx.True(root.GetProperty("max_tokens").GetInt32() is > 0 and <= 512);
            AssertEx.True(!root.GetProperty("chat_template_kwargs").GetProperty("enable_thinking").GetBoolean());

            JsonElement format = root.GetProperty("response_format");
            AssertEx.Equal("json_schema", format.GetProperty("type").GetString());
            var wrapper = format.GetProperty("json_schema");
            AssertEx.Equal("dictation_correction", wrapper.GetProperty("name").GetString());
            AssertEx.True(wrapper.GetProperty("strict").GetBoolean());
            JsonElement schema = wrapper.GetProperty("schema");
            AssertEx.Equal("object", schema.GetProperty("type").GetString());
            AssertEx.Equal("string", schema.GetProperty("properties").GetProperty("corrected_text").GetProperty("type").GetString());
            AssertEx.Equal("corrected_text", schema.GetProperty("required")[0].GetString());
            AssertEx.True(!schema.GetProperty("additionalProperties").GetBoolean());

            JsonElement messages = root.GetProperty("messages");
            AssertEx.Equal(2, messages.GetArrayLength());
            AssertEx.Equal("system", messages[0].GetProperty("role").GetString());
            AssertEx.Equal("user", messages[1].GetProperty("role").GetString());
            string input = messages[1].GetProperty("content").GetString()!;
            AssertEx.True(input.Contains("สวัสดี", StringComparison.Ordinal));
            using var prompt = JsonDocument.Parse(input);
            AssertEx.Equal(Text, prompt.RootElement.GetProperty("text").GetString());
            var terms = prompt.RootElement.GetProperty("protected_terms");
            AssertEx.Equal(1, terms.GetArrayLength());
            AssertEx.Equal("OpenAI", terms[0].GetString());
            return Completion("""{"corrected_text":"สวัสดี OpenAI เจอกันวันพุธ"}""");
        }));

        var client = new DictationCorrectionClient(http, Endpoint, LocalKey);
        AssertEx.Equal("สวัสดี OpenAI เจอกันวันพุธ", await client.CorrectAsync(Text, ["OpenAI", "unused"], default));
        AssertEx.Equal(1, requests);
    }

    private static async Task FallsBackOnInvalidResults()
    {
        foreach (string content in new[]
        {
            "not json", """{"corrected_text":" "}""", """{"corrected_text":"สวัสดี OpenAI","extra":true}""",
            """{"corrected_text":"สวัสดี OpenAI","corrected_text":"สวัสดี OpenAI"}""",
            """{"corrected_text":"สวัสดี openai เจอกันวันพุธ"}""",
            """{"corrected_text":"สวัสดี OpenAI เจอกันวันพุธ\n"}""",
            """{"corrected_text":"สวัสดี OpenAI เจอกันวันพุธ\u2028"}""",
            """{"corrected_text":"สวัสดี OpenAI เจอกันวันพุธ\u0000"}""",
            """{"corrected_text":"สวัสดี OpenAI เจอกันวันพุธ\uD800"}""",
            """{"corrected_text":"OpenAI"}""",
            """{"corrected_text":"สวัสดี OpenAI เจอกันวันพุธ for a meeting and discussion we should book a room now"}"""
        })
        {
            await AssertFallback(() => Completion(content));
        }

        await AssertFallback(() => Completion("""{"corrected_text":"สวัสดี OpenAI เจอกันวันพุธ"}""", "length"));
        await AssertFallback(() => Completion("""{"corrected_text":"สวัสดี OpenAI เจอกันวันพุธ"}""", refusal: "declined"));
        await AssertFallback(() => new HttpResponseMessage(HttpStatusCode.ServiceUnavailable));
        await AssertFallback(() => new HttpResponseMessage(HttpStatusCode.OK) { Content = new StringContent(new string('x', 1024 * 1024)) });

        using var transport = new HttpClient(new Stub((_, _) => throw new HttpRequestException(LocalKey)));
        AssertEx.Equal<string?>(null, await new DictationCorrectionClient(transport, Endpoint, LocalKey)
            .CorrectAsync(Text, ["OpenAI"], default));
    }

    private static async Task AssertFallback(Func<HttpResponseMessage> build)
    {
        using var http = new HttpClient(new Stub((_, _) => Task.FromResult(build())));
        var client = new DictationCorrectionClient(http, Endpoint, LocalKey);
        AssertEx.Equal<string?>(null, await client.CorrectAsync(Text, ["OpenAI"], default));
    }

    private static async Task SkipsInvalidInput()
    {
        using var http = new HttpClient(new Stub((_, _) => throw new Exception("Expected no request.")));
        var client = new DictationCorrectionClient(http, Endpoint, LocalKey);
        AssertEx.Equal<string?>(null, await client.CorrectAsync(" ", [], default));
        AssertEx.Equal<string?>(null, await client.CorrectAsync(new string('a', 32768), [], default));

        using var cancellation = new CancellationTokenSource();
        cancellation.Cancel();
        try
        {
            await client.CorrectAsync(Text, ["OpenAI"], cancellation.Token);
            throw new Exception("Expected cancellation.");
        }
        catch (OperationCanceledException)
        {
        }
    }

    private static void RejectsRemoteEndpoint()
    {
        using var http = new HttpClient(new Stub((_, _) => throw new Exception("Expected no request.")));
        foreach (string target in new[] { "https://example.com/v1/chat/completions", "http://localhost/v1/chat/completions" })
        {
            try
            {
                _ = new DictationCorrectionClient(http, new Uri(target), LocalKey);
                throw new Exception("Non-numeric loopback endpoint was accepted.");
            }
            catch (ArgumentException)
            {
            }
        }
    }

    private static HttpResponseMessage Completion(string content, string finishReason = "stop", string? refusal = null) =>
        new(HttpStatusCode.OK)
        {
            Content = new StringContent(JsonSerializer.Serialize(new
            {
                choices = new[] { new { finish_reason = finishReason, message = new { role = "assistant", content, refusal } } }
            }), Encoding.UTF8, "application/json")
        };

    private sealed class Stub(Func<HttpRequestMessage, CancellationToken, Task<HttpResponseMessage>> send) : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken) =>
            send(request, cancellationToken);
    }
}
