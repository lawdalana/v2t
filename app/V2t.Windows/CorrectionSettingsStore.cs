using System.Text.Json;
using V2t.Core;

namespace V2t.Windows;

internal sealed record CorrectionPreferences(CorrectionDictionary Dictionary, bool EnableAi);

internal static class CorrectionSettingsStore
{
    private const int MaxFileBytes = 640 * 1024;

    public static CorrectionPreferences Load(string path)
    {
        if (!File.Exists(path))
            return new CorrectionPreferences(new CorrectionDictionary([]), true);

        using var file = File.OpenRead(path);
        if (file.Length > MaxFileBytes)
            throw new InvalidDataException("The dictionary file is too large.");
        var saved = JsonSerializer.Deserialize<StoredCorrection>(file)
            ?? throw new InvalidDataException("The dictionary file is empty.");
        return new CorrectionPreferences(new CorrectionDictionary(saved.Entries ?? []), saved.EnableAi);
    }

    public static void Save(string path, CorrectionPreferences preferences)
    {
        var saved = new StoredCorrection(preferences.EnableAi, preferences.Dictionary.Entries);
        var payload = JsonSerializer.SerializeToUtf8Bytes(saved);
        if (payload.Length > MaxFileBytes)
            throw new InvalidDataException("The dictionary file is too large.");

        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        var temporary = path + "." + Guid.NewGuid().ToString("N") + ".tmp";
        try
        {
            File.WriteAllBytes(temporary, payload);
            File.Move(temporary, path, overwrite: true);
        }
        finally
        {
            File.Delete(temporary);
        }
    }

    private sealed record StoredCorrection(bool EnableAi, IReadOnlyList<DictionaryEntry>? Entries);
}
