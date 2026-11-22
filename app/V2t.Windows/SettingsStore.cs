using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using V2t.Core;

namespace V2t.Windows;

internal sealed record DictationSettings(
    SpeechProvider Provider = SpeechProvider.Local,
    string? Language = TranscriptionClient.MixedThaiEnglish,
    string OpenAiKey = "",
    string OpenRouterKey = "",
    bool EnableMaiBackup = true,
    ShortcutBinding? Shortcut = null)
{
    internal ShortcutBinding Hotkey => Shortcut is { IsValid: true } saved ? saved : ShortcutBinding.Default;

    public string? ApiKey => Provider switch
    {
        SpeechProvider.OpenAI => OpenAiKey,
        SpeechProvider.OpenRouter => OpenRouterKey,
        _ => null
    };
}

internal static class SettingsStore
{
    public static readonly string DirectoryPath = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "v2t");
    private static readonly string SettingsFile = Path.Combine(DirectoryPath, "settings.json");

    public static DictationSettings Load() => Load(SettingsFile);

    internal static DictationSettings Load(string path)
    {
        if (!File.Exists(path))
            return new DictationSettings();

        var saved = JsonSerializer.Deserialize<StoredSettings>(File.ReadAllBytes(path))
            ?? throw new InvalidDataException("The settings file is empty.");
        if (!Enum.IsDefined(saved.Provider) || saved.Language is not (null or "th" or "en" or TranscriptionClient.MixedThaiEnglish))
            throw new InvalidDataException("The saved provider or language is invalid.");
        return new DictationSettings(saved.Provider, saved.Language,
            Unprotect(saved.OpenAiKey), Unprotect(saved.OpenRouterKey), saved.EnableMaiBackup ?? false, saved.Shortcut);
    }

    public static void Save(DictationSettings settings) => Save(SettingsFile, settings);

    internal static void Save(string path, DictationSettings settings)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(path))!);
        var saved = new StoredSettings(settings.Provider, settings.Language,
            Protect(settings.OpenAiKey), Protect(settings.OpenRouterKey), settings.EnableMaiBackup, settings.Hotkey);
        var temporary = path + ".tmp";
        try
        {
            File.WriteAllBytes(temporary, JsonSerializer.SerializeToUtf8Bytes(saved));
            File.Move(temporary, path, overwrite: true);
        }
        finally
        {
            File.Delete(temporary);
        }
    }

    private static string? Protect(string value) => string.IsNullOrWhiteSpace(value) ? null :
        Convert.ToBase64String(ProtectedData.Protect(Encoding.UTF8.GetBytes(value), null, DataProtectionScope.CurrentUser));

    private static string Unprotect(string? value) => value is null ? "" : Encoding.UTF8.GetString(
        ProtectedData.Unprotect(Convert.FromBase64String(value), null, DataProtectionScope.CurrentUser));

    private sealed record StoredSettings(
        SpeechProvider Provider,
        string? Language,
        string? OpenAiKey,
        string? OpenRouterKey,
        bool? EnableMaiBackup,
        ShortcutBinding? Shortcut);
}
