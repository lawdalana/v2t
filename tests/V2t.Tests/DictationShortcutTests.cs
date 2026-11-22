using V2t.Windows;

namespace V2t.Tests;

internal static class DictationShortcutTests
{
    private const uint LeftShift = 0xA0;
    private const uint RightShift = 0xA1;
    private const uint LeftControl = 0xA2;
    private const uint RightControl = 0xA3;
    private const uint RightAlt = 0xA5;
    private const uint Tab = 0x09;
    private const uint F8 = 0x77;
    private const uint K = 'K';
    private static readonly ShortcutBinding ControlShiftK = new(ShortcutModifiers.Control | ShortcutModifiers.Shift, K);

    public static void Run()
    {
        StartsInEitherOrderAndFinishesOnce();
        WaitsForBothModifiersToRelease();
        CancelsOtherKeyboardShortcuts();
        SkipsWhenAnotherKeyIsHeld();
        ResetsWhenTheSessionChanges();
        RecoversFromMissedKeyRelease();
        CancelsRecordingAfterMissedChordRelease();
        ValidatesAndLabelsBindings();
        ConsumesAcceptedMainKeyUntilRelease();
        DeclinedAndPrematureMainNeverConsume();
        SupportsStandaloneFunctionAndAlt();
        CancelsOnConflictWithoutLeakingMainKey();
        WaitsForConsumedKeyEvenWhenNativeStateDoesNotSeeIt();
        RebindingCancelsOldChord();
        Console.WriteLine("DictationShortcut: 14 tests passed");
    }

    private static void StartsInEitherOrderAndFinishesOnce()
    {
        Verify(new DictationShortcut(),
            (F8, true, ShortcutAction.None), (F8, false, ShortcutAction.None),
            (LeftControl, true, ShortcutAction.None), (RightShift, true, ShortcutAction.Start),
            (RightShift, true, ShortcutAction.None), (LeftControl, false, ShortcutAction.None),
            (RightShift, false, ShortcutAction.Finish), (RightShift, false, ShortcutAction.None),
            (LeftShift, true, ShortcutAction.None), (RightControl, true, ShortcutAction.Start),
            (LeftShift, false, ShortcutAction.None), (RightControl, false, ShortcutAction.Finish));
    }

    private static void WaitsForBothModifiersToRelease()
    {
        Verify(new DictationShortcut(),
            (LeftControl, true, ShortcutAction.None), (RightShift, true, ShortcutAction.Start),
            (RightControl, true, ShortcutAction.None), (LeftShift, true, ShortcutAction.None),
            (LeftControl, false, ShortcutAction.None), (RightControl, false, ShortcutAction.None),
            (LeftShift, false, ShortcutAction.None), (RightShift, false, ShortcutAction.Finish));
    }

    private static void CancelsOtherKeyboardShortcuts()
    {
        Verify(new DictationShortcut(),
            (LeftControl, true, ShortcutAction.None), (LeftShift, true, ShortcutAction.Start),
            (Tab, true, ShortcutAction.Cancel), (Tab, false, ShortcutAction.None),
            (LeftShift, false, ShortcutAction.None), (LeftShift, true, ShortcutAction.None),
            (LeftControl, false, ShortcutAction.None), (LeftShift, false, ShortcutAction.None),
            (LeftControl, true, ShortcutAction.None), (LeftShift, true, ShortcutAction.Start),
            (LeftShift, false, ShortcutAction.None), (LeftControl, false, ShortcutAction.Finish));
    }

    private static void SkipsWhenAnotherKeyIsHeld()
    {
        Verify(new DictationShortcut(),
            (Tab, true, ShortcutAction.None), (LeftControl, true, ShortcutAction.None),
            (LeftShift, true, ShortcutAction.None), (Tab, false, ShortcutAction.None),
            (RightControl, true, ShortcutAction.None), (LeftControl, false, ShortcutAction.None),
            (RightControl, false, ShortcutAction.None), (LeftShift, false, ShortcutAction.None));
    }

    private static void ResetsWhenTheSessionChanges()
    {
        var shortcut = new DictationShortcut();
        Verify(shortcut, (LeftControl, true, ShortcutAction.None), (RightShift, true, ShortcutAction.Start));
        AssertEx.Equal(ShortcutAction.Cancel, shortcut.Reset());
        AssertEx.Equal(ShortcutAction.None, shortcut.Reset());
        Verify(shortcut,
            (LeftControl, false, ShortcutAction.None), (RightShift, false, ShortcutAction.None),
            (LeftControl, true, ShortcutAction.None), (RightShift, true, ShortcutAction.Start),
            (RightShift, false, ShortcutAction.None), (LeftControl, false, ShortcutAction.Finish));
    }

    private static void RecoversFromMissedKeyRelease()
    {
        var shortcut = new DictationShortcut();
        AssertEx.Equal(ShortcutAction.None, shortcut.Observe(Tab, down: true));
        AssertEx.Equal(ShortcutAction.None, shortcut.Observe(LeftControl, down: true, _ => false));
        AssertEx.Equal(ShortcutAction.Start, shortcut.Observe(RightShift, down: true, key => key == LeftControl));
        Verify(shortcut, (RightShift, false, ShortcutAction.None), (LeftControl, false, ShortcutAction.Finish));

        AssertEx.Equal(ShortcutAction.None, shortcut.Observe(Tab, down: true));
        AssertEx.Equal(ShortcutAction.None, shortcut.Observe(LeftControl, down: true, key => key == Tab));
        AssertEx.Equal(ShortcutAction.None, shortcut.Observe(RightShift, down: true, key => key == Tab || key == LeftControl));
    }

    private static void CancelsRecordingAfterMissedChordRelease()
    {
        var shortcut = new DictationShortcut();
        Verify(shortcut, (LeftControl, true, ShortcutAction.None), (RightShift, true, ShortcutAction.Start));
        AssertEx.Equal(ShortcutAction.Cancel, shortcut.Observe(LeftControl, down: true, _ => false));
        AssertEx.Equal(ShortcutAction.Start, shortcut.Observe(RightShift, down: true, key => key == LeftControl));
        Verify(shortcut, (RightShift, false, ShortcutAction.None), (LeftControl, false, ShortcutAction.Finish));
    }

    private static void ValidatesAndLabelsBindings()
    {
        AssertEx.True(ShortcutBinding.Default.IsValid);
        AssertEx.Equal("Ctrl+Shift", ShortcutBinding.Default.Label);
        AssertEx.True(ControlShiftK.IsValid);
        AssertEx.Equal("Ctrl+Shift+K", ControlShiftK.Label);
        AssertEx.Equal("Alt+Space", new ShortcutBinding(ShortcutModifiers.Alt, 0x20).Label);
        AssertEx.Equal("F8", new ShortcutBinding(ShortcutModifiers.None, F8).Label);
        AssertEx.True(new ShortcutBinding(ShortcutModifiers.Shift, '7').IsValid);
        AssertEx.True(!new ShortcutBinding(ShortcutModifiers.Control, 0).IsValid);
        AssertEx.True(!new ShortcutBinding(ShortcutModifiers.None, K).IsValid);
        AssertEx.True(!new ShortcutBinding(ShortcutModifiers.Control, 0x1B).IsValid);
        AssertEx.True(!new ShortcutBinding(ShortcutModifiers.Control, 0x5B).IsValid);
        AssertEx.True(!new ShortcutBinding((ShortcutModifiers)8, F8).IsValid);
        AssertEx.Equal("{\"Modifiers\":3,\"Key\":75}", System.Text.Json.JsonSerializer.Serialize(ControlShiftK));
    }

    private static void ConsumesAcceptedMainKeyUntilRelease()
    {
        var shortcut = new DictationShortcut(ControlShiftK);
        AssertEx.Equal(new ShortcutObservation(ShortcutAction.None, false), Observe(shortcut, LeftControl, true));
        AssertEx.Equal(new ShortcutObservation(ShortcutAction.None, false), Observe(shortcut, RightShift, true));
        AssertEx.Equal(new ShortcutObservation(ShortcutAction.Start, true), Observe(shortcut, K, true));
        AssertEx.Equal(new ShortcutObservation(ShortcutAction.None, true), Observe(shortcut, K, true));
        AssertEx.Equal(new ShortcutObservation(ShortcutAction.None, false), Observe(shortcut, LeftControl, false));
        AssertEx.Equal(new ShortcutObservation(ShortcutAction.None, false), Observe(shortcut, RightShift, false));
        AssertEx.Equal(new ShortcutObservation(ShortcutAction.Finish, true), Observe(shortcut, K, false));
        AssertEx.Equal(new ShortcutObservation(ShortcutAction.None, false), Observe(shortcut, K, false));
    }

    private static void DeclinedAndPrematureMainNeverConsume()
    {
        var shortcut = new DictationShortcut(ControlShiftK);
        int requests = 0;
        bool Decline() { requests++; return false; }

        AssertEx.Equal(default, shortcut.Observe(K, true, null, Decline));
        AssertEx.Equal(default, shortcut.Observe(LeftControl, true, null, Decline));
        AssertEx.Equal(default, shortcut.Observe(LeftShift, true, null, Decline));
        AssertEx.Equal(default, shortcut.Observe(K, true, null, Decline));
        AssertEx.Equal(0, requests);
        AssertEx.Equal(default, shortcut.Observe(K, false, null, Decline));
        AssertEx.Equal(default, shortcut.Observe(K, true, null, Decline));
        AssertEx.Equal(default, shortcut.Observe(K, true, null, Decline));
        AssertEx.Equal(1, requests);
        AssertEx.Equal(default, shortcut.Observe(K, false, null, Decline));
        Verify(shortcut, (LeftControl, false, ShortcutAction.None), (LeftShift, false, ShortcutAction.None));
    }

    private static void SupportsStandaloneFunctionAndAlt()
    {
        var standalone = new DictationShortcut(new ShortcutBinding(ShortcutModifiers.None, F8));
        AssertEx.Equal(new ShortcutObservation(ShortcutAction.Start, true), Observe(standalone, F8, true));
        AssertEx.Equal(new ShortcutObservation(ShortcutAction.Finish, true), Observe(standalone, F8, false));
        var alt = new DictationShortcut(new ShortcutBinding(ShortcutModifiers.Alt, K));
        Verify(alt, (RightAlt, true, ShortcutAction.None), (K, true, ShortcutAction.Start),
            (K, false, ShortcutAction.None), (RightAlt, false, ShortcutAction.Finish));
        var extraModifiers = new DictationShortcut(ControlShiftK);
        Verify(extraModifiers, (RightAlt, true, ShortcutAction.None), (LeftControl, true, ShortcutAction.None),
            (LeftShift, true, ShortcutAction.None), (K, true, ShortcutAction.None));
    }

    private static void CancelsOnConflictWithoutLeakingMainKey()
    {
        var shortcut = new DictationShortcut(ControlShiftK);
        Verify(shortcut, (LeftControl, true, ShortcutAction.None), (LeftShift, true, ShortcutAction.None));
        AssertEx.Equal(new ShortcutObservation(ShortcutAction.Start, true), Observe(shortcut, K, true));
        AssertEx.Equal(new ShortcutObservation(ShortcutAction.Cancel, false), Observe(shortcut, Tab, true));
        AssertEx.Equal(new ShortcutObservation(ShortcutAction.None, true), Observe(shortcut, K, true));
        AssertEx.Equal(new ShortcutObservation(ShortcutAction.None, true), Observe(shortcut, K, false));
        Verify(shortcut, (Tab, false, ShortcutAction.None), (K, true, ShortcutAction.None),
            (K, false, ShortcutAction.None), (LeftShift, false, ShortcutAction.None), (LeftControl, false, ShortcutAction.None));
    }

    private static void WaitsForConsumedKeyEvenWhenNativeStateDoesNotSeeIt()
    {
        var shortcut = new DictationShortcut(new ShortcutBinding(ShortcutModifiers.None, F8));
        Verify(shortcut, (F8, true, ShortcutAction.Start));
        AssertEx.True(!shortcut.KeysReleased(_ => false));
        Verify(shortcut, (F8, false, ShortcutAction.Finish));
        AssertEx.True(shortcut.KeysReleased(_ => false));
        AssertEx.True(!shortcut.KeysReleased(key => key == F8));
    }

    private static void RebindingCancelsOldChord()
    {
        var shortcut = new DictationShortcut(ControlShiftK);
        Verify(shortcut, (LeftControl, true, ShortcutAction.None), (LeftShift, true, ShortcutAction.None),
            (K, true, ShortcutAction.Start));
        AssertEx.Equal(ShortcutAction.Cancel, shortcut.SetBinding(new ShortcutBinding(ShortcutModifiers.None, F8)));
        AssertEx.Equal(new ShortcutObservation(ShortcutAction.None, false), Observe(shortcut, F8, true));
        AssertEx.Equal(new ShortcutObservation(ShortcutAction.None, true), Observe(shortcut, K, false));
        Verify(shortcut, (F8, true, ShortcutAction.None), (F8, false, ShortcutAction.None),
            (LeftShift, false, ShortcutAction.None), (LeftControl, false, ShortcutAction.None),
            (F8, true, ShortcutAction.Start), (F8, false, ShortcutAction.Finish));
    }

    private static ShortcutObservation Observe(DictationShortcut shortcut, uint key, bool down) =>
        shortcut.Observe(key, down, null, () => true);

    private static void Verify(DictationShortcut shortcut, params (uint Key, bool Down, ShortcutAction Action)[] steps)
    {
        foreach (var step in steps)
            AssertEx.Equal(step.Action, shortcut.Observe(step.Key, step.Down));
    }
}
