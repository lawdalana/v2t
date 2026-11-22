namespace V2t.Core;

public readonly record struct SpeechResourceSnapshot(
    ulong? AvailableRamBytes,
    ulong? AvailableVramBytes,
    uint? GpuUtilization,
    bool ForegroundGame);

public enum SpeechResourceReason
{
    None,
    Game,
    LowRam,
    LowVram,
    BusyGpu
}

public sealed class SpeechResourceRouting
{
    private const ulong GiB = 1024UL * 1024 * 1024;
    private const ulong RamThreshold = 2 * GiB;
    private const ulong VramThreshold = 3 * GiB / 2;
    private const ulong RamRecovery = 3 * GiB;
    private const ulong VramRecovery = 2 * GiB;
    private const uint GpuThreshold = 50;
    private const int RequiredBusySamples = 2;
    private const int RequiredHealthySamples = 3;

    private int busySamples;
    private int healthySamples;

    public SpeechResourceReason Current { get; private set; }

    public SpeechResourceReason Update(SpeechResourceSnapshot snapshot)
    {
        busySamples = snapshot.GpuUtilization >= GpuThreshold
            ? Math.Min(busySamples + 1, RequiredBusySamples)
            : 0;

        var reason = GetReason(snapshot);
        if (reason != SpeechResourceReason.None)
        {
            healthySamples = 0;
            Current = reason;
        }
        else if (Current != SpeechResourceReason.None)
        {
            var healthy = !(snapshot.AvailableRamBytes < RamRecovery) &&
                          !(snapshot.AvailableVramBytes < VramRecovery) &&
                          !(snapshot.GpuUtilization >= GpuThreshold);
            healthySamples = healthy ? healthySamples + 1 : 0;
            if (healthySamples >= RequiredHealthySamples)
                Reset();
        }
        return Current;
    }

    public void Reset()
    {
        Current = SpeechResourceReason.None;
        healthySamples = 0;
        busySamples = 0;
    }

    private SpeechResourceReason GetReason(SpeechResourceSnapshot snapshot)
    {
        if (snapshot.ForegroundGame)
            return SpeechResourceReason.Game;
        if (snapshot.AvailableRamBytes < RamThreshold)
            return SpeechResourceReason.LowRam;
        if (snapshot.AvailableVramBytes < VramThreshold)
            return SpeechResourceReason.LowVram;
        return busySamples >= RequiredBusySamples ? SpeechResourceReason.BusyGpu : SpeechResourceReason.None;
    }
}
