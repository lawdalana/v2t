using System.ComponentModel;
using System.Runtime.InteropServices;

namespace V2t.Windows;

public readonly record struct FocusTarget(nint Window, nint Control)
{
    internal bool Matches(FocusTarget current) =>
        Window != 0 && Window == current.Window && (Control == 0 || Control == current.Control);
}

public sealed class WindowsInput : IDisposable
{
    private const int KeyboardLowLevel = 13;
    private const uint Extended = 0x01;
    private const uint Injected = 0x10;
    private const int KeyDown = 0x0100;
    private const int KeyUp = 0x0101;
    private const int SystemKeyDown = 0x0104;
    private const int SystemKeyUp = 0x0105;
    private const uint InputKeyboard = 1;
    private const uint Unicode = 4;
    private const uint Release = 2;

    private readonly Func<bool> tryPressed;
    private readonly Action released;
    private readonly Action cancelled;
    private readonly KeyboardCallback callback;
    private readonly DictationShortcut shortcut;
    private nint hook;

    // Install and dispose on the owner thread while it has a Windows message loop.
    // The supplied callbacks should capture focus and schedule work without blocking.
    public WindowsInput(Action pressed, Action released, Action cancelled)
        : this(ShortcutBinding.Default, Accept(pressed), released, cancelled) { }

    internal WindowsInput(ShortcutBinding binding, Func<bool> tryPressed, Action released, Action cancelled)
    {
        shortcut = new DictationShortcut(binding);
        this.tryPressed = tryPressed;
        this.released = released;
        this.cancelled = cancelled;
        callback = ObserveKeyboard;
        hook = SetWindowsHookExW(KeyboardLowLevel, callback, GetModuleHandleW(null), 0);
        if (hook == 0)
            throw new Win32Exception(Marshal.GetLastPInvokeError(), $"Could not register the {binding.Label} shortcut.");
    }

    private static Func<bool> Accept(Action pressed)
    {
        ArgumentNullException.ThrowIfNull(pressed);
        return () =>
        {
            pressed();
            return true;
        };
    }

    private nint ObserveKeyboard(int code, nint message, nint data)
    {
        var action = (int)message;
        if (code == 0 && action is KeyDown or KeyUp or SystemKeyDown or SystemKeyUp)
        {
            var key = Marshal.PtrToStructure<KeyboardEvent>(data);
            if ((key.Flags & Injected) == 0)
            {
                var observation = shortcut.Observe(SidedKey(key), action is KeyDown or SystemKeyDown, IsKeyDown, tryPressed);
                switch (observation.Action)
                {
                    case ShortcutAction.Finish: released(); break;
                    case ShortcutAction.Cancel: cancelled(); break;
                }
                if (observation.Consume)
                    return 1;
            }
        }

        return CallNextHookEx(hook, code, message, data);
    }

    private static uint SidedKey(KeyboardEvent key) => key.KeyCode switch
    {
        0x10 => key.ScanCode == 0x36 ? DictationShortcut.RightShift : DictationShortcut.LeftShift,
        0x11 => (key.Flags & Extended) != 0 ? DictationShortcut.RightControl : DictationShortcut.LeftControl,
        0x12 => (key.Flags & Extended) != 0 ? DictationShortcut.RightAlt : DictationShortcut.LeftAlt,
        _ => key.KeyCode
    };

    internal void SetShortcut(ShortcutBinding binding)
    {
        if (shortcut.SetBinding(binding) == ShortcutAction.Cancel)
            cancelled();
    }

    internal void ResetShortcut()
    {
        if (shortcut.Reset() == ShortcutAction.Cancel)
            cancelled();
    }

    public static FocusTarget CaptureFocus()
    {
        nint window = GetForegroundWindow();
        if (window == 0)
            return default;

        uint thread = GetWindowThreadProcessId(window, 0);
        var info = new GuiThreadInfo { Size = (uint)Marshal.SizeOf<GuiThreadInfo>() };
        nint control = thread != 0 && GetGUIThreadInfo(thread, ref info) ? info.Focus : 0;
        return new FocusTarget(window, control);
    }

    public static bool TryInsert(FocusTarget target, string text)
    {
        if (string.IsNullOrEmpty(text))
            return false;

        Input[] input = CreateUnicodeInput(text);
        return !ModifiersAreDown() && target.Matches(CaptureFocus()) &&
            SendInput((uint)input.Length, input, Marshal.SizeOf<Input>()) == input.Length;
    }

    private static bool ModifiersAreDown() => GetAsyncKeyState(0x10) < 0 || // Shift
        GetAsyncKeyState(0x11) < 0 || GetAsyncKeyState(0x12) < 0 ||          // Ctrl, Alt
        GetAsyncKeyState(0x5B) < 0 || GetAsyncKeyState(0x5C) < 0;            // Windows keys

    private static bool IsKeyDown(uint key) => GetAsyncKeyState((int)key) < 0;

    internal bool DictationKeysReleased() => shortcut.KeysReleased(IsKeyDown);

    internal static Input[] CreateUnicodeInput(string text)
    {
        var input = new Input[text.Length * 2];
        for (int index = 0; index < text.Length; index++)
        {
            input[index * 2] = UnicodeInput(text[index], Unicode);
            input[index * 2 + 1] = UnicodeInput(text[index], Unicode | Release);
        }
        return input;
    }

    private static Input UnicodeInput(char codeUnit, uint flags) => new()
    {
        Type = InputKeyboard,
        Value = new InputValue { Keyboard = new KeyboardInput { ScanCode = codeUnit, Flags = flags } },
    };

    public void Dispose()
    {
        if (hook != 0)
        {
            UnhookWindowsHookEx(hook);
            hook = 0;
        }
    }

    [UnmanagedFunctionPointer(CallingConvention.Winapi)]
    private delegate nint KeyboardCallback(int code, nint message, nint data);

    [StructLayout(LayoutKind.Sequential)]
    private struct KeyboardEvent
    {
        public uint KeyCode, ScanCode, Flags, Time;
        public nuint ExtraInfo;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct GuiThreadInfo
    {
        public uint Size, Flags;
        public nint Active, Focus, Capture, MenuOwner, MoveSize, Caret;
        public int CaretLeft, CaretTop, CaretRight, CaretBottom;
    }

    [StructLayout(LayoutKind.Sequential)]
    internal struct Input
    {
        public uint Type;
        public InputValue Value;
    }

    [StructLayout(LayoutKind.Explicit)]
    internal struct InputValue
    {
        [FieldOffset(0)] public KeyboardInput Keyboard;
        // The native union's size and alignment are set by MOUSEINPUT.
        [FieldOffset(0)] public MouseInput Mouse;
    }

    [StructLayout(LayoutKind.Sequential)]
    internal struct KeyboardInput
    {
        public ushort VirtualKey, ScanCode;
        public uint Flags, Time;
        public nuint ExtraInfo;
    }

    [StructLayout(LayoutKind.Sequential)]
    internal struct MouseInput
    {
        public int X, Y;
        public uint Data, Flags, Time;
        public nuint ExtraInfo;
    }

    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode, ExactSpelling = true)]
    private static extern nint GetModuleHandleW(string? name);

    [DllImport("user32.dll", SetLastError = true, ExactSpelling = true)]
    private static extern nint SetWindowsHookExW(int type, KeyboardCallback callback, nint module, uint thread);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool UnhookWindowsHookEx(nint hook);

    [DllImport("user32.dll")]
    private static extern nint CallNextHookEx(nint hook, int code, nint message, nint data);

    [DllImport("user32.dll")]
    private static extern short GetAsyncKeyState(int key);

    [DllImport("user32.dll")]
    private static extern nint GetForegroundWindow();

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(nint window, nint processId);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetGUIThreadInfo(uint thread, ref GuiThreadInfo info);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern uint SendInput(uint count, [In] Input[] input, int size);
}
