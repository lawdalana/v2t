using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;
using V2t.Core;

namespace V2t.Windows;

internal sealed class WindowsResourceProbe : IDisposable
{
    private const uint QueryLimitedInformation = 0x1000;
    private const int InsufficientBuffer = 122;
    private const int ExclusiveDirect3DFullScreen = 3;
    private const int NvmlSuccess = 0;
    private const string NvmlLibrary = "nvml.dll";

    private readonly object sync = new();
    private bool gpuChecked;
    private bool nvmlInitialized;
    private bool disposed;
    private nint gpu;

    internal SpeechResourceSnapshot Capture(nint foregroundWindow)
    {
        lock (sync)
        {
            if (disposed)
                return default;

            if (!gpuChecked)
                InitializeGpu();

            return new SpeechResourceSnapshot(
                GetAvailableRam(), GetAvailableVram(), GetGpuUtilization(), IsForegroundGame(foregroundWindow));
        }
    }

    private void InitializeGpu()
    {
        gpuChecked = true;
        try
        {
            nvmlInitialized = NvmlInit() == NvmlSuccess;
            if (nvmlInitialized && NvmlGetDevice(0, out var device) == NvmlSuccess)
                gpu = device;
        }
        catch (Exception error) when (IsMissingNvml(error))
        {
            // Machines without the NVIDIA driver still report RAM and game state.
        }
    }

    private static ulong? GetAvailableRam()
    {
        var status = new MemoryStatus { Length = (uint)Marshal.SizeOf<MemoryStatus>() };
        return GlobalMemoryStatusEx(ref status) ? status.AvailablePhysical : null;
    }

    private ulong? GetAvailableVram()
    {
        if (gpu == 0)
            return null;

        try
        {
            return NvmlGetMemory(gpu, out var memory) == NvmlSuccess && memory.Free <= memory.Total
                ? memory.Free
                : null;
        }
        catch (Exception error) when (IsMissingNvml(error))
        {
            return null;
        }
    }

    private uint? GetGpuUtilization()
    {
        if (gpu == 0)
            return null;

        try
        {
            return NvmlGetUtilization(gpu, out var utilization) == NvmlSuccess && utilization.Gpu <= 100
                ? utilization.Gpu
                : null;
        }
        catch (Exception error) when (IsMissingNvml(error))
        {
            return null;
        }
    }

    private static bool IsMissingNvml(Exception error) =>
        error is DllNotFoundException or EntryPointNotFoundException or BadImageFormatException;

    private static bool IsForegroundGame(nint window)
    {
        if (window == 0)
            return false;

        if (GetForegroundWindow() == window &&
            SHQueryUserNotificationState(out var notificationState) >= 0 &&
            notificationState == ExclusiveDirect3DFullScreen)
        {
            return true;
        }

        if (GetWindowThreadProcessId(window, out uint processId) == 0)
            return false;

        using var process = OpenProcess(QueryLimitedInformation, false, processId);
        if (process.IsInvalid)
            return false;

        var path = new StringBuilder(512);
        uint length = (uint)path.Capacity;
        if (!QueryFullProcessImageNameW(process, 0, path, ref length))
        {
            if (Marshal.GetLastPInvokeError() != InsufficientBuffer)
                return false;

            path.EnsureCapacity(32_768);
            length = (uint)path.Capacity;
            if (!QueryFullProcessImageNameW(process, 0, path, ref length))
                return false;
        }

        return IsGamePath(path.ToString());
    }

    internal static bool IsGamePath(string path) =>
        path.Contains(@"\steamapps\common\", StringComparison.OrdinalIgnoreCase) ||
        (path.Contains(@"\Epic Games\", StringComparison.OrdinalIgnoreCase) &&
            !path.Contains(@"\Epic Games\Launcher\", StringComparison.OrdinalIgnoreCase) &&
            !path.Contains(@"\Epic Games\UE_", StringComparison.OrdinalIgnoreCase)) ||
        (path.Contains(@"\Riot Games\", StringComparison.OrdinalIgnoreCase) &&
            !path.Contains(@"\Riot Games\Riot Client\", StringComparison.OrdinalIgnoreCase)) ||
        path.Contains(@"\GOG Galaxy\Games\", StringComparison.OrdinalIgnoreCase) ||
        path.Contains(@"\GOG Games\", StringComparison.OrdinalIgnoreCase);

    public void Dispose()
    {
        lock (sync)
        {
            if (disposed)
                return;
            disposed = true;

            if (nvmlInitialized)
            {
                try
                {
                    _ = NvmlShutdown();
                }
                catch (Exception error) when (IsMissingNvml(error))
                {
                    // The driver does not expose shutdown after initialization.
                }
            }
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MemoryStatus
    {
        public uint Length;
        public uint MemoryLoad;
        public ulong TotalPhysical;
        public ulong AvailablePhysical;
        public ulong TotalPageFile;
        public ulong AvailablePageFile;
        public ulong TotalVirtual;
        public ulong AvailableVirtual;
        public ulong AvailableExtendedVirtual;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct NvmlMemory
    {
        public ulong Total;
        public ulong Free;
        public ulong Used;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct NvmlUtilization
    {
        public uint Gpu;
        public uint Memory;
    }

    [DllImport("kernel32.dll", ExactSpelling = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GlobalMemoryStatusEx(ref MemoryStatus status);

    [DllImport("shell32.dll", ExactSpelling = true)]
    private static extern int SHQueryUserNotificationState(out int state);

    [DllImport("user32.dll", ExactSpelling = true)]
    private static extern nint GetForegroundWindow();

    [DllImport("user32.dll", ExactSpelling = true)]
    private static extern uint GetWindowThreadProcessId(nint window, out uint processId);

    [DllImport("kernel32.dll", ExactSpelling = true, SetLastError = true)]
    private static extern SafeProcessHandle OpenProcess(uint access, [MarshalAs(UnmanagedType.Bool)] bool inherit, uint processId);

    [DllImport("kernel32.dll", ExactSpelling = true, CharSet = CharSet.Unicode, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool QueryFullProcessImageNameW(
        SafeProcessHandle process, uint flags, StringBuilder executable, ref uint size);

    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [DllImport(NvmlLibrary, EntryPoint = "nvmlInit_v2", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern int NvmlInit();

    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [DllImport(NvmlLibrary, EntryPoint = "nvmlDeviceGetHandleByIndex_v2", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern int NvmlGetDevice(uint index, out nint device);

    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [DllImport(NvmlLibrary, EntryPoint = "nvmlDeviceGetMemoryInfo", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern int NvmlGetMemory(nint device, out NvmlMemory memory);

    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [DllImport(NvmlLibrary, EntryPoint = "nvmlDeviceGetUtilizationRates", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern int NvmlGetUtilization(nint device, out NvmlUtilization utilization);

    [DefaultDllImportSearchPaths(DllImportSearchPath.System32)]
    [DllImport(NvmlLibrary, EntryPoint = "nvmlShutdown", ExactSpelling = true, CallingConvention = CallingConvention.Cdecl)]
    private static extern int NvmlShutdown();
}
