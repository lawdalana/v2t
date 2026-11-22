namespace V2t.Windows;

internal static class AppIcon
{
    private const string ResourceName = "V2t.App.ico";

    internal static Icon LoadTrayIcon() => Load(SystemInformation.SmallIconSize);

    internal static void ApplyTo(Form window)
    {
        var icon = Load(SystemInformation.IconSize);
        window.Disposed += (_, _) => icon.Dispose();
        window.Icon = icon;
    }

    private static Icon Load(Size size)
    {
        using var source = typeof(AppIcon).Assembly.GetManifestResourceStream(ResourceName)
            ?? throw new InvalidOperationException("The embedded v2t icon could not be found.");
        // Icon copies all encoded sizes, so WinForms can select a small variant for the window.
        return new Icon(source, size);
    }
}
