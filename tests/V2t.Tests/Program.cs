using System.Security.Cryptography;
using V2t.Core;
using V2t.Tests;
using V2t.Windows;

using var http = new HttpClient(new TestDownloadHandler());
var directory = Directory.CreateTempSubdirectory("v2t-test-");
try
{
    var source = new Uri("https://download.invalid/model");
    var path = Path.Combine(directory.FullName, "model.bin");
    var payload = new byte[] { 1, 2, 3 };
    var hash = Convert.ToHexString(SHA256.HashData(payload));

    await AssetDownload.SaveAsync(http, source, path, hash, null, CancellationToken.None);
    AssertEx.True(File.ReadAllBytes(path).SequenceEqual(payload));
    try
    {
        await AssetDownload.SaveAsync(http, source, path, new string('0', 64), null, CancellationToken.None);
        throw new InvalidOperationException("Unexpected success for a bad checksum.");
    }
    catch (InvalidDataException)
    {
        AssertEx.True(File.ReadAllBytes(path).SequenceEqual(payload));
        AssertEx.Equal(1, directory.GetFiles().Length);
    }
    using var cancelled = new CancellationTokenSource();
    await cancelled.CancelAsync();
    try
    {
        await AssetDownload.SaveAsync(http, source, path, hash, null, cancelled.Token);
        throw new InvalidOperationException("Unexpected success after cancellation.");
    }
    catch (OperationCanceledException)
    {
        AssertEx.True(File.ReadAllBytes(path).SequenceEqual(payload));
        AssertEx.Equal(1, directory.GetFiles().Length);
    }
}
finally
{
    directory.Delete(recursive: true);
}
Console.WriteLine("AssetDownload: 3 tests passed");
await TranscriptionTests.RunAsync();
await SpeechFallbackTests.RunAsync();
await SpeechResourceRoutingTests.RunAsync();
CorrectionDictionaryTests.Run();
await DictationCorrectionTests.RunAsync();
var correctionDirectory = Directory.CreateTempSubdirectory("v2t-dictionary-test-");
try
{
    var path = Path.Combine(correctionDirectory.FullName, "correction.json");
    var initial = CorrectionSettingsStore.Load(path);
    AssertEx.True(initial.EnableAi);
    AssertEx.Equal(0, initial.Dictionary.Entries.Count);
    var preferences = new CorrectionPreferences(new CorrectionDictionary([new("โอเพ่นเราเตอร์", "OpenRouter")]), false);
    CorrectionSettingsStore.Save(path, preferences);
    var loaded = CorrectionSettingsStore.Load(path);
    AssertEx.True(!loaded.EnableAi);
    AssertEx.Equal("ลองใช้ OpenRouter", loaded.Dictionary.Apply("ลองใช้ โอเพ่นเราเตอร์").Text);
    CorrectionSettingsStore.Save(path, initial);
    AssertEx.Equal(0, CorrectionSettingsStore.Load(path).Dictionary.Entries.Count);
    AssertEx.Equal(1, correctionDirectory.GetFiles().Length);
    var large = new CorrectionDictionary(Enumerable.Range(0, 300).Select(index =>
        new DictionaryEntry(new string('ก', 157) + index.ToString("D3"), new string('ข', 160))));
    CorrectionSettingsStore.Save(path, new CorrectionPreferences(large, true));
    AssertEx.Equal(300, CorrectionSettingsStore.Load(path).Dictionary.Entries.Count);
    Console.WriteLine("Dictionary persistence: 7 tests passed");
}
finally
{
    correctionDirectory.Delete(recursive: true);
}
AssertEx.Equal("สวัสดี Windows 🙂 Next", DictationText.ForInsertion(" สวัสดี Windows 🙂\r\nNext\t\0 "));
AssertEx.Equal("", DictationText.ForInsertion("\r\n\0\t"));
AssertEx.Equal("one two", DictationText.ForInsertion("one\u2028two"));
Console.WriteLine("DictationText: 3 tests passed");
NativeInputTests.Run();
SpeechAudioGateTests.Run();
OpenRouterKeySourceTests.Run();
UsageLogTests.Run();
DictationShortcutTests.Run();
AssertEx.Equal(LocalModel.Typhoon, LocalModel.PreferredFor("th"));
AssertEx.Equal(LocalModel.Typhoon, LocalModel.PreferredFor(TranscriptionClient.MixedThaiEnglish));
AssertEx.Equal(LocalModel.Whisper, LocalModel.PreferredFor("en"));
AssertEx.Equal(LocalModel.Whisper, LocalModel.PreferredFor(null));
AssertEx.Equal(LocalModel.Typhoon, LocalModel.AvailableFor("th", _ => true));
AssertEx.Equal(LocalModel.Whisper, LocalModel.AvailableFor("th", model => model == LocalModel.Whisper));
AssertEx.Equal(LocalModel.Whisper, LocalModel.AvailableFor("en", model => model == LocalModel.Typhoon));
AssertEx.True(!LocalModel.Typhoon.UseMixedWhisperPrompt);
Console.WriteLine("Local model routing: 8 tests passed");

sealed class TestDownloadHandler : HttpMessageHandler
{
    protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        => Task.FromResult(new HttpResponseMessage(System.Net.HttpStatusCode.OK)
        {
            Content = new ByteArrayContent([1, 2, 3])
        });
}
