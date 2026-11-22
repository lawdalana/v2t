using System.Globalization;

namespace V2t.Windows;

internal sealed class DailyUsageLog(string legacyPath)
{
    private const string DateFormat = "yyyy-MM-dd";
    private const int PreviousDaysToKeep = 6;
    private readonly UsageLog legacy = new(legacyPath);
    private readonly object dailyGate = new();
    private readonly object maintenanceGate = new();
    private DateOnly? dailyCleanupDate;
    private DateOnly? legacyCleanupDate;
    private bool dailyCleanupComplete;

    internal bool TryAppend(UsageEntry entry)
    {
        var date = DateOnly.FromDateTime(entry.StartedAt.Date);
        lock (dailyGate)
        {
            // A dictation may have started before sleep and finished after housekeeping ran.
            if (dailyCleanupDate is { } maintained && date < FirstRetainedDate(maintained))
                return true;

            try
            {
                var name = date.ToString(DateFormat, CultureInfo.InvariantCulture) + ".csv";
                return new UsageLog(Path.Combine(DailyDirectory(), name)).TryAppend(entry);
            }
            catch (Exception error) when (UsageLog.IsFileError(error))
            {
                return false;
            }
        }
    }

    internal bool TryCleanup(DateTimeOffset now)
    {
        var today = DateOnly.FromDateTime(now.Date);
        var cutoff = FirstRetainedDate(today);
        lock (maintenanceGate)
        {
            bool dailyClean = CleanupDaily(today, cutoff);
            if (legacyCleanupDate != today && legacy.TryPruneBefore(cutoff))
                legacyCleanupDate = today;

            return dailyClean && legacyCleanupDate == today;
        }
    }

    private bool CleanupDaily(DateOnly today, DateOnly cutoff)
    {
        lock (dailyGate)
        {
            if (dailyCleanupDate != today)
            {
                dailyCleanupDate = today;
                dailyCleanupComplete = false;
            }

            if (!dailyCleanupComplete)
                dailyCleanupComplete = TryDeleteExpiredFiles(cutoff);
            return dailyCleanupComplete;
        }
    }

    private bool TryDeleteExpiredFiles(DateOnly cutoff)
    {
        bool complete = true;
        try
        {
            foreach (string file in Directory.EnumerateFiles(DailyDirectory(), "*.csv"))
            {
                if (!Path.GetExtension(file).Equals(".csv", StringComparison.Ordinal) ||
                    !DateOnly.TryParseExact(Path.GetFileNameWithoutExtension(file), DateFormat,
                        CultureInfo.InvariantCulture, DateTimeStyles.None, out var date) || date >= cutoff)
                    continue;

                try { File.Delete(file); }
                catch (Exception error) when (UsageLog.IsFileError(error)) { complete = false; }
            }
        }
        catch (DirectoryNotFoundException)
        {
            // No daily log has been created yet.
        }
        catch (Exception error) when (UsageLog.IsFileError(error))
        {
            return false;
        }
        return complete;
    }

    private string DailyDirectory() => Path.Combine(Path.GetDirectoryName(Path.GetFullPath(legacyPath))!, "usage");

    private static DateOnly FirstRetainedDate(DateOnly today) =>
        DateOnly.FromDayNumber(Math.Max(0, today.DayNumber - PreviousDaysToKeep));
}
