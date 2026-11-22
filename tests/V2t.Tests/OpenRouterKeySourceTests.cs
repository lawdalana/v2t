using V2t.Windows;

namespace V2t.Tests;

internal static class OpenRouterKeySourceTests
{
    public static void Run()
    {
        var directory = Directory.CreateTempSubdirectory("v2t-key-source-");
        var first = "sk-or-v1-" + new string('a', 30);
        var second = "sk-or-v1-" + new string('b', 30);
        try
        {
            var publish = Directory.CreateDirectory(Path.Combine(directory.FullName, "publish")).FullName;
            var dotenv = Path.Combine(directory.FullName, ".env");
            File.WriteAllText(dotenv, $"OPENAI_API_KEY={first}\n# OPENROUTER_API_KEY={first}\nexport OPENROUTER_KEY_TOKEN=\"{second}\" # comment\n");
            AssertEx.Equal(second, OpenRouterKeySource.Resolve(null, publish, _ => null));
            AssertEx.Equal(first, OpenRouterKeySource.Resolve($" {first} ", publish, _ => second));
            AssertEx.Equal<string?>(null, OpenRouterKeySource.Resolve("malformed-saved-key", publish, _ => second));
            AssertEx.Equal(first, OpenRouterKeySource.Resolve(null, publish,
                name => name == "OPENROUTER_API_KEY" ? first : null));

            var appEnv = Path.Combine(publish, ".env");
            File.WriteAllText(appEnv, $"OPENROUTER_API_KEY={first}");
            AssertEx.Equal(first, OpenRouterKeySource.Resolve(null, publish, _ => null));
            File.WriteAllText(appEnv, "OPENROUTER_API_KEY=sk-proj-not-openrouter-token-whatever");
            AssertEx.Equal(second, OpenRouterKeySource.Resolve(null, publish, _ => null));
            var standalone = Directory.CreateDirectory(Path.Combine(directory.FullName, "standalone")).FullName;
            AssertEx.Equal<string?>(null, OpenRouterKeySource.Resolve(null, standalone, _ => null));

            File.WriteAllText(appEnv, $"OPENROUTER_KEY={first} unexpected");
            File.Delete(dotenv);
            AssertEx.Equal<string?>(null, OpenRouterKeySource.Resolve(null, publish, _ => null));
            Console.WriteLine("OpenRouter key source: 8 tests passed");
        }
        finally
        {
            directory.Delete(recursive: true);
        }
    }
}
