using System.Numerics;

namespace V2t.Windows;

[Flags]
internal enum ShortcutModifiers
{
    None = 0,
    Control = 1,
    Shift = 2,
    Alt = 4
}

internal readonly record struct ShortcutBinding(ShortcutModifiers Modifiers, uint Key)
{
    private const ShortcutModifiers Supported = ShortcutModifiers.Control | ShortcutModifiers.Shift | ShortcutModifiers.Alt;
    public static ShortcutBinding Default => new(ShortcutModifiers.Control | ShortcutModifiers.Shift, 0);

    internal bool IsValid => (Modifiers & ~Supported) == 0 && (Key == 0
        ? BitOperations.PopCount((uint)Modifiers) >= 2
        : IsFunctionKey(Key) || Modifiers != ShortcutModifiers.None && IsSupportedKey(Key));

    internal string Label
    {
        get
        {
            if (!IsValid)
                return "";
            var parts = new List<string>(4);
            if (Has(ShortcutModifiers.Control)) parts.Add("Ctrl");
            if (Has(ShortcutModifiers.Shift)) parts.Add("Shift");
            if (Has(ShortcutModifiers.Alt)) parts.Add("Alt");
            if (Key != 0)
                parts.Add(IsFunctionKey(Key) ? $"F{Key - 0x6F}" : Key == 0x20 ? "Space" : ((char)Key).ToString());
            return string.Join('+', parts);
        }
    }

    internal bool Has(ShortcutModifiers modifier) => (Modifiers & modifier) != 0;
    private static bool IsFunctionKey(uint key) => key is >= 0x70 and <= 0x7B;
    private static bool IsSupportedKey(uint key) => key is 0x20 or (>= 'A' and <= 'Z') or (>= '0' and <= '9');
}

internal enum ShortcutAction
{
    None,
    Start,
    Finish,
    Cancel
}

internal readonly record struct ShortcutObservation(ShortcutAction Action, bool Consume);

internal sealed class DictationShortcut
{
    internal static string Label => ShortcutBinding.Default.Label;
    internal const uint LeftShift = 0xA0;
    internal const uint RightShift = 0xA1;
    internal const uint LeftControl = 0xA2;
    internal const uint RightControl = 0xA3;
    internal const uint LeftAlt = 0xA4;
    internal const uint RightAlt = 0xA5;
    private static readonly Func<bool> AlwaysAccept = () => true;
    private readonly HashSet<uint> heldKeys = [];
    private ShortcutBinding binding;
    private uint suppressedKey;
    private bool recording;
    private bool blocked;

    internal DictationShortcut() : this(ShortcutBinding.Default) { }

    internal DictationShortcut(ShortcutBinding binding)
    {
        Validate(binding);
        this.binding = binding;
    }

    internal ShortcutAction Observe(uint key, bool down, Func<uint, bool>? isKeyDown = null) =>
        Observe(key, down, isKeyDown, AlwaysAccept).Action;

    internal ShortcutObservation Observe(uint key, bool down, Func<uint, bool>? isKeyDown, Func<bool> tryPressed)
    {
        bool consume = key != 0 && key == suppressedKey;
        if (consume && !down)
            suppressedKey = 0;

        bool chordKey = IsChordKey(key);
        if (down && chordKey && isKeyDown is not null)
        {
            // A swallowed main key may never appear in the OS's asynchronous key state.
            heldKeys.RemoveWhere(held => held != suppressedKey && !isKeyDown(held));
            if (!heldKeys.Any(IsChordKey))
            {
                blocked = false;
                if (recording)
                {
                    heldKeys.Add(key);
                    return new ShortcutObservation(End(ShortcutAction.Cancel), consume);
                }
            }
        }

        // The main key must be a new down event, even if recording state was reset while it was held.
        if (down && key != 0 && key == binding.Key && !consume && isKeyDown is not null && isKeyDown(key))
        {
            heldKeys.Add(key);
            return default;
        }
        if (!(down ? heldKeys.Add(key) : heldKeys.Remove(key)))
            return new ShortcutObservation(ShortcutAction.None, consume);

        if (!heldKeys.Any(IsChordKey))
        {
            blocked = false;
            return new ShortcutObservation(End(ShortcutAction.Finish), consume);
        }

        if (down && !chordKey)
        {
            blocked = true;
            return new ShortcutObservation(End(ShortcutAction.Cancel), consume);
        }

        if (down && recording && key != 0 && key == binding.Key)
        {
            suppressedKey = key;
            return new ShortcutObservation(ShortcutAction.None, true);
        }
        if (!down || blocked || recording || (binding.Key != 0 && key != binding.Key) || !RequiredModifiersHeld())
            return new ShortcutObservation(ShortcutAction.None, consume);

        blocked = suppressedKey != 0 || heldKeys.Any(held => !IsChordKey(held)) ||
            UnexpectedModifierHeld(isKeyDown) || !tryPressed();
        if (blocked)
            return new ShortcutObservation(ShortcutAction.None, consume);

        recording = true;
        suppressedKey = binding.Key;
        return new ShortcutObservation(ShortcutAction.Start, suppressedKey != 0);
    }

    internal ShortcutAction SetBinding(ShortcutBinding updated)
    {
        Validate(updated);
        if (updated == binding)
            return ShortcutAction.None;
        binding = updated;
        return ClearChord();
    }

    internal ShortcutAction Reset()
    {
        suppressedKey = 0;
        return ClearChord();
    }

    internal bool KeysReleased(Func<uint, bool> isDown) =>
        (binding.Key == 0 || binding.Key != suppressedKey && !isDown(binding.Key)) &&
        (!binding.Has(ShortcutModifiers.Control) || !isDown(0x11)) &&
        (!binding.Has(ShortcutModifiers.Shift) || !isDown(0x10)) &&
        (!binding.Has(ShortcutModifiers.Alt) || !isDown(0x12));

    private ShortcutAction ClearChord()
    {
        heldKeys.Clear();
        blocked = false;
        return End(ShortcutAction.Cancel);
    }

    private ShortcutAction End(ShortcutAction action)
    {
        if (!recording)
            return ShortcutAction.None;
        recording = false;
        return action;
    }

    private bool RequiredModifiersHeld()
    {
        var modifiers = ShortcutModifiers.None;
        foreach (uint held in heldKeys)
            modifiers |= GetModifier(held);
        return (modifiers & binding.Modifiers) == binding.Modifiers;
    }

    private bool UnexpectedModifierHeld(Func<uint, bool>? isDown) => isDown is not null &&
        ((!binding.Has(ShortcutModifiers.Control) && isDown(0x11)) ||
         (!binding.Has(ShortcutModifiers.Shift) && isDown(0x10)) ||
         (!binding.Has(ShortcutModifiers.Alt) && isDown(0x12)) || isDown(0x5B) || isDown(0x5C));

    private bool IsChordKey(uint key) => (binding.Key != 0 && key == binding.Key) || binding.Has(GetModifier(key));

    private static ShortcutModifiers GetModifier(uint key) => key switch
    {
        LeftShift or RightShift or 0x10 => ShortcutModifiers.Shift,
        LeftControl or RightControl or 0x11 => ShortcutModifiers.Control,
        LeftAlt or RightAlt or 0x12 => ShortcutModifiers.Alt,
        _ => ShortcutModifiers.None
    };

    private static void Validate(ShortcutBinding binding)
    {
        if (!binding.IsValid)
            throw new ArgumentException("Choose a valid dictation shortcut.", nameof(binding));
    }
}
