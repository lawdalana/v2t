using System.Security.Cryptography;

namespace V2t.Core;

public static class AssetDownload
{
    public static async Task SaveAsync(
        HttpClient http,
        Uri source,
        string destination,
        string sha256,
        IProgress<int>? progress,
        CancellationToken cancellationToken)
    {
        var temporary = destination + "." + Guid.NewGuid().ToString("N") + ".part";

        try
        {
            using var response = await http.GetAsync(source, HttpCompletionOption.ResponseHeadersRead, cancellationToken);
            response.EnsureSuccessStatusCode();
            await using var incoming = await response.Content.ReadAsStreamAsync(cancellationToken);
            using var hash = IncrementalHash.CreateHash(HashAlgorithmName.SHA256);
            var size = response.Content.Headers.ContentLength;
            var buffer = new byte[128 * 1024];
            long received = 0;
            var previousPercent = -1;

            await using (var output = new FileStream(temporary, FileMode.CreateNew, FileAccess.Write,
                FileShare.None, buffer.Length, FileOptions.Asynchronous | FileOptions.SequentialScan))
            {
                int count;
                while ((count = await incoming.ReadAsync(buffer, cancellationToken)) > 0)
                {
                    await output.WriteAsync(buffer.AsMemory(0, count), cancellationToken);
                    hash.AppendData(buffer.AsSpan(0, count));
                    received += count;

                    if (size is > 0 && progress is not null)
                    {
                        var percent = (int)Math.Min(100, 100d * received / size.Value);
                        if (percent != previousPercent)
                            progress.Report(percent);
                        previousPercent = percent;
                    }
                }
            }

            cancellationToken.ThrowIfCancellationRequested();
            if (!Convert.ToHexString(hash.GetHashAndReset()).Equals(sha256, StringComparison.OrdinalIgnoreCase))
                throw new InvalidDataException("Downloaded file failed its checksum.");

            File.Move(temporary, destination, overwrite: true);
        }
        finally
        {
            File.Delete(temporary);
        }
    }
}
