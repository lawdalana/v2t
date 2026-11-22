using System.Collections.Concurrent;
using System.ComponentModel;
using V2t.Core;

namespace V2t.Tests;

internal static class SpeechFallbackTests
{
    private static readonly TimeSpan NormalTimeout = TimeSpan.FromSeconds(5);

    internal static async Task RunAsync()
    {
        await LocalSuccessNeverUsesBackup();
        await OperationalFailuresUseBackupOnce();
        await PrimaryTimeoutCancelsNoncooperativeTask();
        await NoBackupPreservesLocalErrors();
        await CallerCancellationNeverStartsBackup();
        await CallerCancellationStopsWaitingForBackup();
        await InvariantErrorsNeverUseBackup();
        await BackupErrorIsSurfacedOnItsOwn();
        BackupStartsOnCallerContext();
        Console.WriteLine("SpeechFallback: 9 tests passed");
    }

    private static async Task LocalSuccessNeverUsesBackup()
    {
        foreach (var text in new[] { "สวัสดี Windows", "", " " })
        {
            var result = await SpeechFallback.RunAsync(_ => Task.FromResult(text), UnexpectedBackup, NormalTimeout, default);
            AssertEx.Equal(new SpeechFallbackResult(text, UsedBackup: false), result);
        }
    }

    private static async Task OperationalFailuresUseBackupOnce()
    {
        Exception[] failures =
        [
            new InvalidOperationException(), new Win32Exception(), new HttpRequestException(),
            new IOException(), new InvalidDataException(), new TimeoutException()
        ];
        foreach (var failure in failures)
        {
            int localCalls = 0, backupCalls = 0;
            var result = await SpeechFallback.RunAsync(_ =>
            {
                localCalls++;
                return Task.FromException<string>(failure);
            }, _ =>
            {
                backupCalls++;
                return Task.FromResult("Backup transcript");
            }, NormalTimeout, default);
            AssertEx.Equal(new SpeechFallbackResult("Backup transcript", UsedBackup: true), result);
            AssertEx.Equal(1, localCalls);
            AssertEx.Equal(1, backupCalls);
        }
    }

    private static async Task PrimaryTimeoutCancelsNoncooperativeTask()
    {
        using var caller = new CancellationTokenSource();
        using var pending = new CancellationTokenSource();
        var neverFinishes = new TaskCompletionSource<string>(TaskCreationOptions.RunContinuationsAsynchronously);
        CancellationToken localToken = default;
        int calls = 0;
        try
        {
            var result = await SpeechFallback.RunAsync(token =>
            {
                localToken = token;
                return neverFinishes.Task;
            }, token =>
            {
                AssertEx.Equal(caller.Token, token);
                AssertEx.True(!token.IsCancellationRequested);
                calls++;
                return Task.FromResult("Fallback after timeout");
            }, TimeSpan.FromMilliseconds(50), caller.Token).WaitAsync(NormalTimeout);
            AssertEx.Equal(new SpeechFallbackResult("Fallback after timeout", UsedBackup: true), result);
            AssertEx.True(localToken.IsCancellationRequested);
            AssertEx.Equal(1, calls);
        }
        finally
        {
            pending.Cancel();
            neverFinishes.TrySetCanceled(pending.Token);
        }
    }

    private static async Task NoBackupPreservesLocalErrors()
    {
        var failure = new IOException("Local failed");
        var reported = await ThrowsAsync<IOException>(() =>
            SpeechFallback.RunAsync(_ => Task.FromException<string>(failure), null, NormalTimeout, default));
        AssertEx.True(ReferenceEquals(failure, reported));

        var neverFinishes = new TaskCompletionSource<string>();
        try
        {
            var timeout = await ThrowsAsync<TimeoutException>(() =>
                SpeechFallback.RunAsync(_ => neverFinishes.Task, null, TimeSpan.FromMilliseconds(50), default)
                    .WaitAsync(NormalTimeout, CancellationToken.None));
            AssertEx.True(timeout.Message.Contains("Local transcription", StringComparison.Ordinal));
        }
        finally
        {
            neverFinishes.TrySetCanceled();
        }
    }

    private static async Task CallerCancellationNeverStartsBackup()
    {
        using var caller = new CancellationTokenSource();
        var neverFinishes = new TaskCompletionSource<string>();
        try
        {
            var operation = SpeechFallback.RunAsync(_ => neverFinishes.Task, UnexpectedBackup, NormalTimeout, caller.Token);
            caller.Cancel();
            var error = await ThrowsAsync<OperationCanceledException>(() => operation.WaitAsync(NormalTimeout));
            AssertEx.Equal(caller.Token, error.CancellationToken);
            await ThrowsAsync<OperationCanceledException>(() =>
                SpeechFallback.RunAsync(UnexpectedBackup, UnexpectedBackup, NormalTimeout, caller.Token));
        }
        finally
        {
            neverFinishes.TrySetCanceled();
        }
    }

    private static async Task CallerCancellationStopsWaitingForBackup()
    {
        using var caller = new CancellationTokenSource();
        var neverFinishes = new TaskCompletionSource<string>();
        try
        {
            var operation = SpeechFallback.RunAsync(_ => throw new HttpRequestException(),
                _ => neverFinishes.Task, NormalTimeout, caller.Token);
            caller.Cancel();
            var error = await ThrowsAsync<OperationCanceledException>(() => operation.WaitAsync(NormalTimeout));
            AssertEx.Equal(caller.Token, error.CancellationToken);
        }
        finally
        {
            neverFinishes.TrySetCanceled();
        }
    }

    private static async Task InvariantErrorsNeverUseBackup()
    {
        await ThrowsAsync<ArgumentException>(() => SpeechFallback.RunAsync(_ => throw new ArgumentException(),
            UnexpectedBackup, NormalTimeout, default));
        await ThrowsAsync<ObjectDisposedException>(() => SpeechFallback.RunAsync(_ => throw new ObjectDisposedException("local"),
            UnexpectedBackup, NormalTimeout, default));
        await ThrowsAsync<OperationCanceledException>(() => SpeechFallback.RunAsync(_ => throw new OperationCanceledException(),
            UnexpectedBackup, NormalTimeout, default));
        await ThrowsAsync<UnsupportedLocalTranscriptionLanguageException>(() => SpeechFallback.RunAsync(
            _ => throw new UnsupportedLocalTranscriptionLanguageException(), UnexpectedBackup, NormalTimeout, default));
        await ThrowsAsync<ArgumentOutOfRangeException>(() => SpeechFallback.RunAsync(UnexpectedBackup,
            UnexpectedBackup, TimeSpan.Zero, default));
    }

    private static async Task BackupErrorIsSurfacedOnItsOwn()
    {
        var failure = new InvalidOperationException("Backup transcription failed. Check settings.");
        var reported = await ThrowsAsync<InvalidOperationException>(() => SpeechFallback.RunAsync(
            _ => throw new Win32Exception("private native details"), _ => Task.FromException<string>(failure), NormalTimeout, default));
        AssertEx.True(ReferenceEquals(failure, reported));
    }

    private static void BackupStartsOnCallerContext()
    {
        var prior = SynchronizationContext.Current;
        using var context = new TestContext();
        SynchronizationContext.SetSynchronizationContext(context);
        try
        {
            int thread = Environment.CurrentManagedThreadId;
            var local = new TaskCompletionSource<string>(TaskCreationOptions.RunContinuationsAsynchronously);
            var operation = SpeechFallback.RunAsync(_ => local.Task, _ =>
            {
                AssertEx.True(ReferenceEquals(context, SynchronizationContext.Current));
                AssertEx.Equal(thread, Environment.CurrentManagedThreadId);
                return Task.FromResult("Backup");
            }, NormalTimeout, default);
            local.SetException(new IOException());
            context.RunOne();
            AssertEx.Equal(new SpeechFallbackResult("Backup", UsedBackup: true),
                operation.WaitAsync(NormalTimeout).GetAwaiter().GetResult());
        }
        finally
        {
            SynchronizationContext.SetSynchronizationContext(prior);
        }
    }

    private static Task<string> UnexpectedBackup(CancellationToken _) =>
        throw new Exception("Unexpected invocation.");

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
        throw new InvalidOperationException($"Expected {typeof(T).Name}.");
    }

    private sealed class TestContext : SynchronizationContext, IDisposable
    {
        private readonly BlockingCollection<(SendOrPostCallback Callback, object? State)> updates = new();

        public override void Post(SendOrPostCallback callback, object? state) => updates.Add((callback, state));

        public void RunOne()
        {
            if (!updates.TryTake(out var update, NormalTimeout))
                throw new InvalidOperationException("The continuation did not return to the caller context.");
            update.Callback(update.State);
        }

        public void Dispose() => updates.Dispose();
    }
}
