using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;
using System.Runtime.Versioning;
using System.Text;

namespace V2t.Windows;

[SupportedOSPlatform("windows")]
internal sealed class LoginStartup(string folder, string executable)
{
    private const string Argument = "--background";
    private const string Description = "v2t login launcher";
    private const uint RawPath = 4;
    private static readonly Guid ShellLinkClassId = new("00021401-0000-0000-C000-000000000046");
    private string Link => Path.Combine(folder, "v2t.lnk");

    internal static LoginStartup ForCurrentUser()
    {
        string folder = Environment.GetFolderPath(Environment.SpecialFolder.Startup,
            Environment.SpecialFolderOption.DoNotVerify);
        string? executable = Environment.ProcessPath;
        if (string.IsNullOrEmpty(folder) || executable is null ||
            string.Equals(Path.GetFileName(executable), "dotnet.exe", StringComparison.OrdinalIgnoreCase))
            throw new IOException("Windows Startup needs a packaged v2t executable.");
        return new LoginStartup(folder, executable);
    }

    internal bool Enabled => ReadLink() is { } registration &&
        (StartsCurrent(registration) || (Managed(registration) && File.Exists(registration.Executable)));

    internal void SetEnabled(bool enabled)
    {
        var registration = ReadLink();
        if (!enabled)
        {
            if (registration is not null && Owns(registration))
                File.Delete(Link);
            return;
        }

        if (registration is not null)
        {
            if (StartsCurrent(registration))
                return;
            if (!Owns(registration))
                throw new IOException("An existing v2t.lnk in the Windows Startup folder points to a different app.");
        }

        Directory.CreateDirectory(folder);
        string temporary = Path.Combine(folder, $".v2t-{Guid.NewGuid():N}.tmp");
        try
        {
            WriteLink(temporary, executable, Argument, Description);
            File.Move(temporary, Link, overwrite: registration is not null);
        }
        finally
        {
            File.Delete(temporary);
        }
    }

    private bool StartsCurrent(Registration entry) =>
        entry.Arguments.Trim() == Argument &&
        string.Equals(entry.Executable, executable, StringComparison.OrdinalIgnoreCase);

    private bool Owns(Registration entry) =>
        StartsCurrent(entry) || Managed(entry);

    private static bool Managed(Registration entry) =>
        entry.Arguments.Trim() == Argument && entry.Description == Description;

    private Registration? ReadLink()
    {
        FileAttributes attributes;
        try { attributes = File.GetAttributes(Link); }
        catch (FileNotFoundException) { return null; }
        catch (DirectoryNotFoundException) { return null; }
        if ((attributes & (FileAttributes.ReparsePoint | FileAttributes.Directory)) != 0)
            throw new IOException("The existing Windows Startup shortcut is not a regular file.");

        var link = CreateLink();
        try
        {
            ((IPersistFile)link).Load(Link, 0);
            var target = new StringBuilder(32768);
            var arguments = new StringBuilder(1024);
            var description = new StringBuilder(1024);
            link.GetPath(target, target.Capacity, 0, RawPath);
            link.GetArguments(arguments, arguments.Capacity);
            link.GetDescription(description, description.Capacity);
            return new Registration(target.ToString(), arguments.ToString(), description.ToString());
        }
        finally { Marshal.FinalReleaseComObject(link); }
    }

    internal static void WriteLink(string path, string executable, string arguments, string description)
    {
        var link = CreateLink();
        try
        {
            link.SetPath(executable);
            link.SetWorkingDirectory(Path.GetDirectoryName(executable) ?? "");
            link.SetArguments(arguments);
            link.SetDescription(description);
            ((IPersistFile)link).Save(path, false);
        }
        finally { Marshal.FinalReleaseComObject(link); }
    }

    private static IShellLinkW CreateLink() => (IShellLinkW)
        Activator.CreateInstance(Type.GetTypeFromCLSID(ShellLinkClassId, throwOnError: true)!)!;

    private sealed record Registration(string Executable, string Arguments, string Description);

    // The method order is the native IShellLinkW vtable order.
    [ComImport, Guid("000214F9-0000-0000-C000-000000000046")]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IShellLinkW
    {
        void GetPath([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder path, int size, nint fileData, uint flags);
        void GetIDList(out nint itemIdList);
        void SetIDList(nint itemIdList);
        void GetDescription([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder description, int size);
        void SetDescription([MarshalAs(UnmanagedType.LPWStr)] string description);
        void GetWorkingDirectory([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder path, int size);
        void SetWorkingDirectory([MarshalAs(UnmanagedType.LPWStr)] string path);
        void GetArguments([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder arguments, int size);
        void SetArguments([MarshalAs(UnmanagedType.LPWStr)] string arguments);
        void GetHotkey(out ushort hotkey);
        void SetHotkey(ushort hotkey);
        void GetShowCmd(out int command);
        void SetShowCmd(int command);
        void GetIconLocation([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder path, int size, out int index);
        void SetIconLocation([MarshalAs(UnmanagedType.LPWStr)] string path, int index);
        void SetRelativePath([MarshalAs(UnmanagedType.LPWStr)] string path, uint reserved);
        void Resolve(nint window, uint flags);
        void SetPath([MarshalAs(UnmanagedType.LPWStr)] string path);
    }
}
