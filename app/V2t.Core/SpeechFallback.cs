using System.ComponentModel;

namespace V2t.Core;

public sealed record SpeechFallbackResult(string Text, bool UsedBackup);

public static class SpeechFallback
{
    public static async Task<SpeechFallbackResult> RunAsync(
        Func<CancellationToken, Task<string>> local,
        Func<CancellationToken, Task<string>>? backup,
        TimeSpan localTimeout,
        CancellationToken cancellationToken,
        bool preferBackup = false)
    {
        ArgumentNullException.ThrowIfNull(local);
        ArgumentOutOfRangeException.ThrowIfLessThanOrEqual(localTimeout, TimeSpan.Zero);
        cancellationToken.ThrowIfCancellationRequested();
        if (preferBackup && backup is not null)
            return await UseBackupAsync(backup, cancellationToken);

        using var localCancellation = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        localCancellation.CancelAfter(localTimeout);
        try
        {
            var text = await local(localCancellation.Token).WaitAsync(localCancellation.Token);
            cancellationToken.ThrowIfCancellationRequested();
            return new SpeechFallbackResult(text, UsedBackup: false);
        }
        catch (Exception error) when (IsOperational(error) ||
            (error is OperationCanceledException && localCancellation.IsCancellationRequested))
        {
            cancellationToken.ThrowIfCancellationRequested();
            if (backup is null)
            {
                if (error is OperationCanceledException)
                    throw new TimeoutException("Local transcription timed out.");
                throw;
            }
        }

        return await UseBackupAsync(backup, cancellationToken);
    }

    private static async Task<SpeechFallbackResult> UseBackupAsync(
        Func<CancellationToken, Task<string>> backup, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        var text = await backup(cancellationToken).WaitAsync(cancellationToken);
        cancellationToken.ThrowIfCancellationRequested();
        return new SpeechFallbackResult(text, UsedBackup: true);
    }

    private static bool IsOperational(Exception error) => error is
        (InvalidOperationException and not (ObjectDisposedException or UnsupportedLocalTranscriptionLanguageException)) or Win32Exception or
        HttpRequestException or IOException or InvalidDataException or TimeoutException;
}
