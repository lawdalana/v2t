using System.Text.RegularExpressions;

namespace V2t.Windows;

internal static class OpenRouterKeySource
{
    private static readonly string[] Names = ["OPENROUTER_API_KEY", "OPENROUTER_KEY_TOKEN", "OPENROUTER_KEY"];
    private static readonly Regex Assignment = new(
        """^\s*(?:export\s+)?(?<name>OPENROUTER_API_KEY|OPENROUTER_KEY_TOKEN|OPENROUTER_KEY)\s*=\s*(?:"(?<value>sk-or-v1-[A-Za-z0-9_-]{16,})"|'(?<value>sk-or-v1-[A-Za-z0-9_-]{16,})'|(?<value>sk-or-v1-[A-Za-z0-9_-]{16,}))(?:\s+#.*)?\s*$""",
        RegexOptions.CultureInvariant);
    private static readonly Regex Token = new(@"^sk-or-v1-[A-Za-z0-9_-]{16,}$", RegexOptions.CultureInvariant);

    internal static string? Resolve(string? saved, string appDirectory, Func<string, string?>? readEnvironment = null)
    {
        if (!string.IsNullOrWhiteSpace(saved))
            return Valid(saved);

        readEnvironment ??= Environment.GetEnvironmentVariable;
        foreach (var name in Names)
        {
            if (Valid(readEnvironment(name)) is { } environment)
                return environment;
        }

        var app = new DirectoryInfo(appDirectory);
        var adjacent = ReadDotEnv(Path.Combine(app.FullName, ".env"));
        if (adjacent is not null)
            return adjacent;

        return app.Name.Equals("publish", StringComparison.OrdinalIgnoreCase) && app.Parent is { } project
            ? ReadDotEnv(Path.Combine(project.FullName, ".env"))
            : null;
    }

    private static string? Valid(string? value)
    {
        var cleaned = value?.Trim();
        return cleaned is { Length: <= 512 } && Token.IsMatch(cleaned) ? cleaned : null;
    }

    private static string? ReadDotEnv(string path)
    {
        try
        {
            using var file = File.OpenRead(path);
            if (file.Length > 64 * 1024)
                return null;

            using var reader = new StreamReader(file);
            var found = new Dictionary<string, string>(StringComparer.Ordinal);
            while (reader.ReadLine() is { } line)
            {
                var match = Assignment.Match(line);
                if (match.Success && Valid(match.Groups["value"].Value) is { } token)
                    found[match.Groups["name"].Value] = token;
            }

            foreach (var name in Names)
            {
                if (found.TryGetValue(name, out var token))
                    return token;
            }

            return null;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            return null;
        }
    }
}
