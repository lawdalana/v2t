using System.Globalization;
using System.Text;
using V2t.Windows;

namespace V2t.Tests;

internal static class UsageLogTests
{
    private const string Header = "started_at,stage,provider,model,outcome,audio_seconds,elapsed_seconds,billed_seconds,cost_usd,cost_kind";
    private static readonly UsageEntry Example = new(
        new DateTimeOffset(2026, 11, 23, 8, 19, 2, TimeSpan.FromHours(7)),
        "transcription", "OpenRouter", "microsoft/mai-transcribe-2", "success", 1.5, 0.125, 2, 0.00001m, "billed");
    private static readonly DateOnly FirstRetainedDate = new(2026, 11, 17);

    internal static void Run()
    {
        using var directory = new TemporaryDirectory();
        WritesSingleHeaderAndInvariantValues(directory.File("invariant.csv"));
        LeavesUnknownMetadataBlank(directory.File("unknown.csv"));
        SafelyQuotesSpreadsheetValues(directory.File("spreadsheet.csv"));
        RetainsExistingCsv(directory.File("existing.csv"));
        ConcurrentAppendsDoNotInterleave(directory.File("concurrent.csv"));
        ReturnsFalseWithoutDamagingExistingData(directory.File("failure.csv"));
        PrunesOlderRowsAcrossTimeZones(directory.File("mixed-age.csv"));
        RetainsHeaderWhenAllRowsExpire(directory.File("all-expired.csv"));
        LeavesMissingAndRecentLogsUntouched(directory.File("missing"), directory.File("recent.csv"));
        PreservesMultilineAndUnknownRecords(directory.File("multiline.csv"));
        LeavesLockedLogUntouched(directory.File("locked.csv"));
        WritesNewUsageToTheRecordedDate(directory.File("daily-writes/usage.csv"));
        DeletesOnlyExpiredDailyFiles(directory.File("daily-cleanup/usage.csv"));
        TrimsLegacyRowsByRecordedDate(directory.File("legacy-cleanup/usage.csv"));
        CachesOnlySuccessfulCleanup(directory.File("cached-cleanup/usage.csv"));
        Console.WriteLine("UsageLog: 15 tests passed");
    }

    private static void WritesSingleHeaderAndInvariantValues(string path)
    {
        var originalCulture = CultureInfo.CurrentCulture;
        try
        {
            CultureInfo.CurrentCulture = CultureInfo.GetCultureInfo("fr-FR");
            var log = new UsageLog(path);
            AssertEx.True(log.TryAppend(Example));
            AssertEx.True(log.TryAppend(Example with { Stage = "correction", Provider = "ในเครื่อง" }));
        }
        finally
        {
            CultureInfo.CurrentCulture = originalCulture;
        }

        byte[] contents = File.ReadAllBytes(path);
        AssertEx.True(contents.AsSpan().StartsWith(new byte[] { 0xef, 0xbb, 0xbf }));
        AssertEx.Equal(1, Encoding.UTF8.GetString(contents).Count(character => character == '\ufeff'));
        string[] lines = File.ReadAllLines(path);
        AssertEx.Equal(3, lines.Length);
        AssertEx.Equal(Header, lines[0]);
        AssertEx.Equal("2026-11-23T08:19:02.0000000+07:00,\"transcription\",\"OpenRouter\",\"microsoft/mai-transcribe-2\",\"success\",1.5,0.125,2,0.00001,\"billed\"", lines[1]);
        AssertEx.True(lines[2].Contains("\"ในเครื่อง\"", StringComparison.Ordinal));
    }

    private static void LeavesUnknownMetadataBlank(string path)
    {
        AssertEx.True(new UsageLog(path).TryAppend(Example with
        {
            AudioSeconds = null, BilledSeconds = null, CostUsd = null, CostKind = "unknown", Outcome = "failed"
        }));
        var lines = File.ReadAllLines(path);
        AssertEx.Equal(Header, lines[0]);
        AssertEx.True(lines[1].EndsWith("\"failed\",,0.125,,,\"unknown\"", StringComparison.Ordinal));
        AssertEx.True(!lines[0].Contains("api_key", StringComparison.OrdinalIgnoreCase));
        AssertEx.True(!lines[0].Contains("transcript_text", StringComparison.OrdinalIgnoreCase));
        AssertEx.True(!lines[0].Contains("error_message", StringComparison.OrdinalIgnoreCase));
    }

    private static void SafelyQuotesSpreadsheetValues(string path)
    {
        var malicious = Example with
        {
            Stage = "text \"with\", separators;", Provider = "=1+1", Model = "  +SUM(1)",
            Outcome = "@exec", CostKind = "－2"
        };
        AssertEx.True(new UsageLog(path).TryAppend(malicious));
        var row = File.ReadAllLines(path)[1];
        AssertEx.True(row.Contains("\"text \"\"with\"\", separators;\"", StringComparison.Ordinal));
        foreach (var safe in new[] { "\"'=1+1\"", "\"'  +SUM(1)\"", "\"'@exec\"", "\"'－2\"" })
            AssertEx.True(row.Contains(safe, StringComparison.Ordinal), $"Missing safe CSV field {safe}.");
    }

    private static void RetainsExistingCsv(string path)
    {
        byte[] original = "existing,data\r\nolder,row"u8.ToArray();
        File.WriteAllBytes(path, original);
        AssertEx.True(new UsageLog(path).TryAppend(Example));
        AssertEx.True(File.ReadAllBytes(path).AsSpan().StartsWith(original));
        var lines = File.ReadAllLines(path);
        AssertEx.Equal(3, lines.Length);
        AssertEx.Equal("older,row", lines[1]);
    }

    private static void ConcurrentAppendsDoNotInterleave(string path)
    {
        Parallel.For(0, 50, index => AssertEx.True(new UsageLog(path).TryAppend(Example with
        {
            Model = $"model-{index}"
        })));
        var lines = File.ReadAllLines(path);
        AssertEx.Equal(51, lines.Length);
        AssertEx.Equal(Header, lines[0]);
        for (int index = 0; index < 50; index++)
            AssertEx.Equal(1, lines.Count(line => line.Contains($"\"model-{index}\"", StringComparison.Ordinal)));
    }

    private static void ReturnsFalseWithoutDamagingExistingData(string path)
    {
        File.WriteAllText(path, "existing");
        AssertEx.True(!new UsageLog(Path.Combine(path, "usage.csv")).TryAppend(Example));
        AssertEx.Equal("existing", File.ReadAllText(path));
        AssertEx.True(!new UsageLog(Path.GetDirectoryName(path)!).TryAppend(Example));
    }

    private static void PrunesOlderRowsAcrossTimeZones(string path)
    {
        File.WriteAllText(path, Header + "\r\n" +
            "2026-11-16T23:59:59.9999999Z,expired\r\n" +
            "2026-11-17T00:00:00-05:00,boundary\r\n" +
            "2026-11-17T08:19:02.0000001+07:00,recent\r\n" +
            "2026-11-18T01:19:03Z,recent-utc\r\n");

        AssertEx.True(new UsageLog(path).TryPruneBefore(FirstRetainedDate));
        AssertEx.Equal(Header + "\r\n" +
            "2026-11-17T00:00:00-05:00,boundary\r\n" +
            "2026-11-17T08:19:02.0000001+07:00,recent\r\n" +
            "2026-11-18T01:19:03Z,recent-utc\r\n", File.ReadAllText(path));
    }

    private static void RetainsHeaderWhenAllRowsExpire(string path)
    {
        var log = new UsageLog(path);
        AssertEx.True(log.TryAppend(Example with { StartedAt = Example.StartedAt.AddDays(-8) }));
        AssertEx.True(log.TryAppend(Example with { StartedAt = Example.StartedAt.AddDays(-10) }));
        AssertEx.True(log.TryPruneBefore(FirstRetainedDate));
        AssertEx.Equal("\ufeff" + Header + "\r\n", Encoding.UTF8.GetString(File.ReadAllBytes(path)));
        AssertEx.True(log.TryAppend(Example));
        AssertEx.Equal(2, File.ReadAllLines(path).Length);
    }

    private static void LeavesMissingAndRecentLogsUntouched(string directory, string path)
    {
        AssertEx.True(new UsageLog(Path.Combine(directory, "usage.csv")).TryPruneBefore(FirstRetainedDate));
        AssertEx.True(!Directory.Exists(directory));

        var log = new UsageLog(path);
        AssertEx.True(log.TryAppend(Example));
        var bytes = File.ReadAllBytes(path);
        File.SetLastWriteTimeUtc(path, new DateTime(2024, 1, 2, 3, 4, 5, DateTimeKind.Utc));
        var written = File.GetLastWriteTimeUtc(path);
        AssertEx.True(log.TryPruneBefore(FirstRetainedDate));
        AssertEx.Equal(written, File.GetLastWriteTimeUtc(path));
        AssertEx.True(bytes.AsSpan().SequenceEqual(File.ReadAllBytes(path)));
    }

    private static void PreservesMultilineAndUnknownRecords(string path)
    {
        const string recent = "\"2026-11-23T01:19:02Z\",\"multiline \"\"ข้อความ\"\"\r\n2026-10-01T00:00:00Z,inside field\",tail\n";
        const string unknown = "invalid timestamp,keep\r\n2026-10-01T00:00:00,unknown zone\ninvalid,\"unclosed\nfinal line";
        const string old = "2026-11-14T01:19:02Z,\"expired,\r\ncontinued\"\r\n";
        var expected = "\ufeff" + Header + "\n" + recent + unknown;
        File.WriteAllText(path, "\ufeff" + Header + "\n" + old + recent + unknown);

        AssertEx.True(new UsageLog(path).TryPruneBefore(FirstRetainedDate));
        AssertEx.Equal(expected, Encoding.UTF8.GetString(File.ReadAllBytes(path)));
    }

    private static void LeavesLockedLogUntouched(string path)
    {
        var log = new UsageLog(path);
        AssertEx.True(log.TryAppend(Example with { StartedAt = Example.StartedAt.AddDays(-8) }));
        var bytes = File.ReadAllBytes(path);
        using (File.Open(path, FileMode.Open, FileAccess.ReadWrite, FileShare.None))
            AssertEx.True(!log.TryPruneBefore(FirstRetainedDate));
        AssertEx.True(bytes.AsSpan().SequenceEqual(File.ReadAllBytes(path)));
        AssertEx.True(log.TryPruneBefore(FirstRetainedDate));
        AssertEx.Equal(1, File.ReadAllLines(path).Length);
    }

    private static void WritesNewUsageToTheRecordedDate(string path)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        const string legacy = "original legacy data";
        File.WriteAllText(path, legacy);
        var daily = new DailyUsageLog(path);

        AssertEx.True(daily.TryAppend(Example));
        AssertEx.True(daily.TryAppend(Example with { Stage = "correction" }));
        AssertEx.True(daily.TryAppend(Example with { StartedAt = Example.StartedAt.ToOffset(TimeSpan.FromHours(-5)) }));

        var folder = Path.Combine(Path.GetDirectoryName(path)!, "usage");
        string[] today = File.ReadAllLines(Path.Combine(folder, "2026-11-23.csv"));
        AssertEx.Equal(3, today.Length);
        AssertEx.Equal(Header, today[0]);
        AssertEx.True(today[2].Contains("\"correction\"", StringComparison.Ordinal));
        AssertEx.Equal(2, File.ReadAllLines(Path.Combine(folder, "2026-11-22.csv")).Length);
        AssertEx.Equal(legacy, File.ReadAllText(path));
    }

    private static void DeletesOnlyExpiredDailyFiles(string path)
    {
        var folder = Path.Combine(Path.GetDirectoryName(path)!, "usage");
        Directory.CreateDirectory(folder);
        string[] expired = ["2026-11-16.csv", "2025-01-01.csv"];
        string[] retained = ["2026-11-17.csv", "2026-11-23.csv", "2026-11-24.csv", "2026-02-30.csv",
            "2026-11-1.csv", "2026-01-01.CSV", "2026-01-01.csv.tmp", "notes.csv"];
        foreach (var name in expired.Concat(retained))
            File.WriteAllText(Path.Combine(folder, name), name);
        var datedDirectory = Path.Combine(folder, "2024-01-01.csv");
        Directory.CreateDirectory(datedDirectory);

        AssertEx.True(new DailyUsageLog(path).TryCleanup(Example.StartedAt));
        foreach (string name in expired)
            AssertEx.True(!File.Exists(Path.Combine(folder, name)));
        foreach (string name in retained)
            AssertEx.Equal(name, File.ReadAllText(Path.Combine(folder, name)));
        AssertEx.True(Directory.Exists(datedDirectory));
        AssertEx.True(!File.Exists(path));
    }

    private static void TrimsLegacyRowsByRecordedDate(string path)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        const string expired = "2026-11-16T23:59:59-08:00,legacy-old\r\n";
        const string recent = "2026-11-17T00:00:00+14:00,legacy-boundary\r\n";
        const string unknown = "unknown age,keep\r\n";
        var header = "\ufeff" + Header + "\r\n";
        File.WriteAllText(path, header + expired + recent + unknown);

        AssertEx.True(new DailyUsageLog(path).TryCleanup(Example.StartedAt));
        AssertEx.Equal(header + recent + unknown, Encoding.UTF8.GetString(File.ReadAllBytes(path)));
        AssertEx.True(!Directory.Exists(Path.Combine(Path.GetDirectoryName(path)!, "usage")));
    }

    private static void CachesOnlySuccessfulCleanup(string path)
    {
        var daily = new DailyUsageLog(path);
        var legacy = new UsageLog(path);
        AssertEx.True(legacy.TryAppend(Example with { StartedAt = Example.StartedAt.AddDays(-8) }));
        using (File.Open(path, FileMode.Open, FileAccess.ReadWrite, FileShare.None))
            AssertEx.True(!daily.TryCleanup(Example.StartedAt));
        AssertEx.True(daily.TryCleanup(Example.StartedAt));
        AssertEx.Equal(1, File.ReadAllLines(path).Length);

        AssertEx.True(daily.TryAppend(Example with { StartedAt = Example.StartedAt.AddDays(-7) }));
        var folder = Path.Combine(Path.GetDirectoryName(path)!, "usage");
        AssertEx.True(!File.Exists(Path.Combine(folder, "2026-11-16.csv")));
        AssertEx.True(daily.TryAppend(Example with { StartedAt = Example.StartedAt.AddDays(-6) }));
        AssertEx.True(File.Exists(Path.Combine(folder, "2026-11-17.csv")));

        AssertEx.True(legacy.TryAppend(Example with { StartedAt = Example.StartedAt.AddDays(-7) }));
        AssertEx.True(daily.TryCleanup(Example.StartedAt.AddHours(1)));
        AssertEx.Equal(2, File.ReadAllLines(path).Length);
        AssertEx.True(daily.TryCleanup(Example.StartedAt.AddDays(1)));
        AssertEx.Equal(1, File.ReadAllLines(path).Length);
    }

    private sealed class TemporaryDirectory : IDisposable
    {
        private readonly DirectoryInfo directory = Directory.CreateTempSubdirectory("v2t-usage-");

        public string File(string fileName) => Path.Combine(directory.FullName, fileName);
        public void Dispose() => directory.Delete(recursive: true);
    }
}
