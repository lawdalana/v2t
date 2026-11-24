using V2t.Windows;

internal static class Program
{
    [STAThread]
    private static int Main(string[] args)
    {
        bool background = args.Contains("--background");
        using var instance = new Mutex(initiallyOwned: true, "Local\\v2t-desktop", out var firstInstance);
        if (!firstInstance)
        {
            if (!background)
                MessageBox.Show("v2t is already running. Open Settings from the v2t icon in the Windows tray.", "v2t");
            return 0;
        }

        ApplicationConfiguration.Initialize();
        try
        {
            Application.Run(new TrayApp(background));
            return 0;
        }
        catch (System.ComponentModel.Win32Exception error)
        {
            MessageBox.Show($"v2t could not start. {error.Message}", "v2t", MessageBoxButtons.OK, MessageBoxIcon.Error);
            return 1;
        }
    }
}
