namespace V2t.Tests;

internal static class AssertEx
{
    public static void Equal<T>(T expected, T actual)
    {
        if (!EqualityComparer<T>.Default.Equals(expected, actual))
            throw new InvalidOperationException($"Expected {expected}, got {actual}.");
    }

    public static void True(bool value, string? message = null)
    {
        if (!value)
            throw new InvalidOperationException(message ?? "Expected true.");
    }
}
