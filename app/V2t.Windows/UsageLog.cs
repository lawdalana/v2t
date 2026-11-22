using System.Collections.Concurrent;
using System.Globalization;
using System.Security;
using System.Text;

namespace V2t.Windows;

internal sealed record UsageEntry(
    DateTimeOffset StartedAt, string Stage, string Provider, string Model, string Outcome,
    double? AudioSeconds, double ElapsedSeconds, double? BilledSeconds, decimal? CostUsd, string CostKind);

internal sealed class UsageLog(string path)
{
    private const string LineBreak = "\r\n";
    private const string Header = "started_at,stage,provider,model,outcome,audio_seconds,elapsed_seconds,billed_seconds,cost_usd,cost_kind" + LineBreak;
    private static readonly ConcurrentDictionary<string, object> PathGates = new(
        OperatingSystem.IsWindows() ? StringComparer.OrdinalIgnoreCase : StringComparer.Ordinal);
    private static readonly string[] TimestampFormats =
    [
        "yyyy-MM-dd'T'HH:mm:ss.FFFFFFFzzz", "yyyy-MM-dd'T'HH:mm:sszzz",
        "yyyy-MM-dd'T'HH:mm:ss.FFFFFFF'Z'", "yyyy-MM-dd'T'HH:mm:ss'Z'"
    ];

    internal bool TryAppend(UsageEntry entry)
    {
        try
        {
            var fullPath = Path.GetFullPath(path);
            lock (PathGates.GetOrAdd(fullPath, _ => new object()))
            {
                Directory.CreateDirectory(Path.GetDirectoryName(fullPath)!);
                using var file = new FileStream(fullPath, FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.Read, bufferSize: 1);
                long originalLength = file.Length;
                var bytes = Encoding.UTF8.GetBytes(Prefix(file, originalLength) + Format(entry) + LineBreak);
                file.Position = originalLength;
                try
                {
                    file.Write(bytes);
                    file.Flush();
                    return true;
                }
                catch (Exception error) when (IsFileError(error))
                {
                    // Remove an incomplete row if the write started before failing.
                    try { file.SetLength(originalLength); }
                    catch (Exception rollbackError) when (IsFileError(rollbackError)) { }
                    return false;
                }
            }
        }
        catch (Exception error) when (IsFileError(error))
        {
            return false;
        }
    }

    internal bool TryPruneBefore(DateOnly firstRetainedDate)
    {
        try
        {
            var fullPath = Path.GetFullPath(path);
            lock (PathGates.GetOrAdd(fullPath, _ => new object()))
            {
                byte[] original;
                try { original = File.ReadAllBytes(fullPath); }
                catch (Exception error) when (error is FileNotFoundException or DirectoryNotFoundException) { return true; }

                var retained = RetainRecentRecords(original, firstRetainedDate);
                if (retained is null)
                    return true;

                var temporary = fullPath + "." + Guid.NewGuid().ToString("N") + ".tmp";
                try
                {
                    File.WriteAllBytes(temporary, retained.Value.Span);
                    File.Replace(temporary, fullPath, destinationBackupFileName: null);
                }
                finally
                {
                    File.Delete(temporary);
                }
                return true;
            }
        }
        catch (Exception error) when (IsFileError(error))
        {
            return false;
        }
    }

    private static ReadOnlyMemory<byte>? RetainRecentRecords(ReadOnlySpan<byte> original, DateOnly firstRetainedDate)
    {
        byte[]? retained = null;
        int retainedLength = 0;
        int keepFrom = 0;
        int start = original.StartsWith("\ufeff"u8) ? 3 : 0;
        while (start < original.Length && TryReadRecord(original, start, out int next))
        {
            if (TryReadTimestamp(original[start..next], out var time) && DateOnly.FromDateTime(time.Date) < firstRetainedDate)
            {
                retained ??= new byte[original.Length];
                original[keepFrom..start].CopyTo(retained.AsSpan(retainedLength));
                retainedLength += start - keepFrom;
                keepFrom = next;
            }
            start = next;
        }

        if (retained is null)
            return null;

        original[keepFrom..].CopyTo(retained.AsSpan(retainedLength));
        return retained.AsMemory(0, retainedLength + original.Length - keepFrom);
    }

    private static bool TryReadRecord(ReadOnlySpan<byte> original, int start, out int next)
    {
        bool inQuotes = false;
        bool fieldStart = true;
        for (int index = start; index < original.Length; index++)
        {
            byte current = original[index];
            if (inQuotes)
            {
                if (current == (byte)'"')
                {
                    if (index + 1 < original.Length && original[index + 1] == (byte)'"')
                        index++;
                    else
                        inQuotes = false;
                }
                continue;
            }

            if (current is (byte)'\r' or (byte)'\n')
            {
                next = index + 1;
                if (current == (byte)'\r' && next < original.Length && original[next] == (byte)'\n')
                    next++;
                return true;
            }

            inQuotes = fieldStart && current == (byte)'"';
            fieldStart = current == (byte)',';
        }

        next = original.Length;
        return !inQuotes;
    }

    private static bool TryReadTimestamp(ReadOnlySpan<byte> record, out DateTimeOffset time)
    {
        time = default;
        int end = record.IndexOfAny((byte)',', (byte)'\r', (byte)'\n');
        var timestamp = end < 0 ? record : record[..end];
        if (timestamp.Length >= 2 && timestamp[0] == (byte)'"' && timestamp[^1] == (byte)'"')
            timestamp = timestamp[1..^1];
        return timestamp.Length is >= 20 and <= 33 &&
            DateTimeOffset.TryParseExact(Encoding.UTF8.GetString(timestamp), TimestampFormats,
                CultureInfo.InvariantCulture, DateTimeStyles.AssumeUniversal, out time);
    }

    private static string Prefix(FileStream file, long length)
    {
        if (length == 0)
            return "\ufeff" + Header;
        if (length == 3 && file.ReadByte() == 0xef && file.ReadByte() == 0xbb && file.ReadByte() == 0xbf)
            return Header;

        file.Position = length - 1;
        return file.ReadByte() switch
        {
            '\n' => "",
            '\r' => "\n",
            _ => LineBreak
        };
    }

    private static string Format(UsageEntry entry) => string.Join(',',
        entry.StartedAt.ToString("O", CultureInfo.InvariantCulture),
        Quote(entry.Stage), Quote(entry.Provider), Quote(entry.Model), Quote(entry.Outcome),
        Number(entry.AudioSeconds), Number(entry.ElapsedSeconds), Number(entry.BilledSeconds),
        entry.CostUsd?.ToString(CultureInfo.InvariantCulture) ?? "", Quote(entry.CostKind));

    private static string Number(double? value) => value is double number && double.IsFinite(number)
        ? number.ToString("R", CultureInfo.InvariantCulture)
        : "";

    private static string Quote(string value) =>
        "\"" + (HasFormulaPrefix(value) ? "'" : "") + value.Replace("\"", "\"\"", StringComparison.Ordinal) + "\"";

    private static bool HasFormulaPrefix(string value)
    {
        foreach (char character in value)
        {
            if (char.IsControl(character))
                return true;
            if (char.IsWhiteSpace(character) || char.GetUnicodeCategory(character) == UnicodeCategory.Format)
                continue;
            return character is '=' or '+' or '-' or '@' or '＝' or '＋' or '－' or '＠';
        }
        return false;
    }

    internal static bool IsFileError(Exception error) => error is
        IOException or UnauthorizedAccessException or SecurityException or ArgumentException or NotSupportedException;
}
