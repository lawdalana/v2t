using V2t.Core;

namespace V2t.Tests;

internal static class SpeechResourceRoutingTests
{
    private const ulong GiB = 1024UL * 1024 * 1024;
    private static readonly SpeechResourceSnapshot Healthy = new(20 * GiB, 5 * GiB, 30, false);

    internal static async Task RunAsync()
    {
        var routing = new SpeechResourceRouting();
        AssertEx.Equal(SpeechResourceReason.None, routing.Update(Healthy));
        AssertEx.Equal(SpeechResourceReason.Game, routing.Update(Healthy with { ForegroundGame = true }));
        AssertEx.Equal(SpeechResourceReason.Game, routing.Update(Healthy));
        AssertEx.Equal(SpeechResourceReason.Game, routing.Update(Healthy));
        AssertEx.Equal(SpeechResourceReason.None, routing.Update(Healthy));

        AssertEx.Equal(SpeechResourceReason.LowRam, routing.Update(Healthy with { AvailableRamBytes = GiB }));
        for (var i = 0; i < 4; i++)
            AssertEx.Equal(SpeechResourceReason.LowRam, routing.Update(Healthy with { AvailableRamBytes = 5 * GiB / 2 }));
        routing.Reset();
        AssertEx.Equal(SpeechResourceReason.LowVram, routing.Update(Healthy with { AvailableVramBytes = GiB }));

        routing.Reset();
        var busyGpu = Healthy with { GpuUtilization = 50 };
        var belowThreshold = Healthy with { GpuUtilization = 49 };
        AssertEx.Equal(SpeechResourceReason.None, routing.Update(busyGpu));
        AssertEx.Equal(SpeechResourceReason.None, routing.Update(belowThreshold));
        AssertEx.Equal(SpeechResourceReason.None, routing.Update(busyGpu));
        AssertEx.Equal(SpeechResourceReason.BusyGpu, routing.Update(busyGpu));
        for (var i = 0; i < 4; i++)
            AssertEx.Equal(SpeechResourceReason.BusyGpu, routing.Update(busyGpu));
        AssertEx.Equal(SpeechResourceReason.BusyGpu, routing.Update(belowThreshold));
        AssertEx.Equal(SpeechResourceReason.BusyGpu, routing.Update(belowThreshold));
        AssertEx.Equal(SpeechResourceReason.None, routing.Update(belowThreshold));

        routing.Reset();
        AssertEx.Equal(SpeechResourceReason.None, routing.Update(new SpeechResourceSnapshot(null, null, null, false)));

        var localCalls = 0;
        var backupCalls = 0;
        var selected = await SpeechFallback.RunAsync(_ =>
        {
            localCalls++;
            return Task.FromResult("Local");
        }, _ =>
        {
            backupCalls++;
            return Task.FromResult("MAI");
        }, TimeSpan.FromSeconds(5), default, preferBackup: true);
        AssertEx.Equal(new SpeechFallbackResult("MAI", true), selected);
        AssertEx.Equal(0, localCalls);
        AssertEx.Equal(1, backupCalls);
        var withoutKey = await SpeechFallback.RunAsync(_ => Task.FromResult("Local"), null,
            TimeSpan.FromSeconds(5), default, preferBackup: true);
        AssertEx.Equal(new SpeechFallbackResult("Local", false), withoutKey);
        using var canceled = new CancellationTokenSource();
        canceled.Cancel();
        try
        {
            await SpeechFallback.RunAsync(_ => throw new InvalidOperationException("No Local expected."),
                _ => throw new InvalidOperationException("No upload expected."), TimeSpan.FromSeconds(5),
                canceled.Token, preferBackup: true);
            throw new InvalidOperationException("Expected caller cancellation.");
        }
        catch (OperationCanceledException)
        {
            Console.WriteLine("Resource routing and proactive MAI selection passed.");
        }
    }
}
