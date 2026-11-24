using V2t.Windows;

namespace V2t.Tests;

internal static class LoginStartupTests
{
    internal static void Run()
    {
        if (!OperatingSystem.IsWindows())
            return;

        var thread = new Thread(RunInApartment);
        Exception? failure = null;
        void RunInApartment()
        {
            try
            {
                if (OperatingSystem.IsWindows())
                    RoundTrip();
            }
            catch (Exception error) { failure = error; }
        }
        thread.SetApartmentState(ApartmentState.STA);
        thread.Start();
        thread.Join();
        if (failure is not null)
            throw new InvalidOperationException("Windows Startup tests failed.", failure);
        Console.WriteLine("Windows Startup: .NET Shell Link tests passed");
    }

    [System.Runtime.Versioning.SupportedOSPlatform("windows")]
    private static void RoundTrip()
    {
        var directory = Directory.CreateTempSubdirectory("v2t-startup ทดสอบ-");
        try
        {
            string executable = Environment.ProcessPath!;
            string link = Path.Combine(directory.FullName, "v2t.lnk");
            var startup = new LoginStartup(directory.FullName, executable);
            AssertEx.True(!startup.Enabled);
            LoginStartup.WriteLink(link, executable, "--background", "");
            AssertEx.True(startup.Enabled);
            startup.SetEnabled(false);
            AssertEx.True(!File.Exists(link));

            startup.SetEnabled(true);
            AssertEx.True(startup.Enabled);
            startup.SetEnabled(false);
            startup.SetEnabled(false);
            AssertEx.True(!startup.Enabled);

            LoginStartup.WriteLink(link, executable, "--unrelated", "");
            var original = File.ReadAllBytes(link);
            try
            {
                startup.SetEnabled(true);
                throw new InvalidOperationException("An unrelated shortcut should be preserved.");
            }
            catch (IOException)
            {
                startup.SetEnabled(false);
            }
            AssertEx.True(File.ReadAllBytes(link).SequenceEqual(original));
        }
        finally
        {
            directory.Delete(recursive: true);
        }
    }
}
