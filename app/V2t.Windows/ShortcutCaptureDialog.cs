using System.Windows.Forms.Automation;

namespace V2t.Windows;

internal sealed class ShortcutCaptureDialog : Form, IMessageFilter
{
    private const int WmKeyDown = 0x0100;
    private const int WmKeyUp = 0x0101;
    private const int WmSystemKeyDown = 0x0104;
    private const int WmSystemKeyUp = 0x0105;
    private const string Guidance = "Use at least two modifiers, F1–F12 alone, or a modifier with a letter, digit, Space or F key.";

    private readonly TextBox capture = new()
    {
        ReadOnly = true, Dock = DockStyle.Top, AccessibleName = "Shortcut capture"
    };
    private readonly Button useShortcut = new() { Text = "Use shortcut", DialogResult = DialogResult.OK, AutoSize = true };
    private readonly Label feedback = new() { AutoSize = true, LiveSetting = AutomationLiveSetting.Polite };
    private ShortcutModifiers held;
    private ShortcutBinding? modifierOnly;
    private bool pressedOtherKey;
    private bool filtering;

    internal ShortcutCaptureDialog(ShortcutBinding current)
    {
        Text = "Change dictation shortcut";
        StartPosition = FormStartPosition.CenterParent;
        FormBorderStyle = FormBorderStyle.FixedDialog;
        AutoScaleDimensions = new SizeF(96, 96);
        AutoScaleMode = AutoScaleMode.Dpi;
        ClientSize = new Size(474, 220);
        MinimizeBox = false;
        MaximizeBox = false;
        ShowInTaskbar = false;
        AppIcon.ApplyTo(this);
        AcceptButton = useShortcut;

        var cancel = new Button { Text = "Cancel", DialogResult = DialogResult.Cancel, AutoSize = true };
        CancelButton = cancel;
        var restore = new Button { Text = "Restore default", AutoSize = true };
        restore.Click += (_, _) => SelectBinding(ShortcutBinding.Default);
        capture.Leave += (_, _) => ResetHeld();
        Deactivate += (_, _) => ResetHeld();

        var layout = new TableLayoutPanel { Dock = DockStyle.Fill, Padding = new Padding(18), RowCount = 5, ColumnCount = 1 };
        layout.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 100));
        layout.RowStyles.Add(new RowStyle(SizeType.AutoSize));
        layout.RowStyles.Add(new RowStyle(SizeType.AutoSize));
        layout.RowStyles.Add(new RowStyle(SizeType.AutoSize));
        layout.RowStyles.Add(new RowStyle(SizeType.Percent, 100));
        layout.RowStyles.Add(new RowStyle(SizeType.AutoSize));

        layout.Controls.Add(Help("Press the shortcut you want to hold while speaking."), 0, 0);
        layout.Controls.Add(Help(Guidance), 0, 1);
        capture.Margin = new Padding(0, 8, 0, 0);
        layout.Controls.Add(capture, 0, 2);
        feedback.Margin = new Padding(0, 8, 0, 0);
        layout.Controls.Add(feedback, 0, 3);

        var actions = new TableLayoutPanel { AutoSize = true, Dock = DockStyle.Top, RowCount = 1, ColumnCount = 3, Margin = Padding.Empty };
        actions.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 100));
        actions.ColumnStyles.Add(new ColumnStyle(SizeType.AutoSize));
        actions.ColumnStyles.Add(new ColumnStyle(SizeType.AutoSize));
        actions.Controls.Add(restore, 0, 0);
        actions.Controls.Add(cancel, 1, 0);
        actions.Controls.Add(useShortcut, 2, 0);
        layout.Controls.Add(actions, 0, 4);
        Controls.Add(layout);
        SelectBinding(current);
    }

    internal ShortcutBinding? SelectedBinding { get; private set; }

    protected override void OnShown(EventArgs e)
    {
        base.OnShown(e);
        Application.AddMessageFilter(this);
        filtering = true;
        capture.Focus();
    }

    bool IMessageFilter.PreFilterMessage(ref Message message) =>
        capture.IsHandleCreated && capture.Focused && message.HWnd == capture.Handle &&
        (message.Msg is WmKeyDown or WmKeyUp or WmSystemKeyDown or WmSystemKeyUp) &&
        CaptureKey((Keys)message.WParam.ToInt64(), message.Msg is WmKeyDown or WmSystemKeyDown);

    private bool CaptureKey(Keys key, bool down)
    {
        var modifier = key switch
        {
            Keys.ControlKey or Keys.LControlKey or Keys.RControlKey => ShortcutModifiers.Control,
            Keys.ShiftKey or Keys.LShiftKey or Keys.RShiftKey => ShortcutModifiers.Shift,
            Keys.Menu or Keys.LMenu or Keys.RMenu => ShortcutModifiers.Alt,
            _ => ShortcutModifiers.None
        };

        if (modifier != ShortcutModifiers.None)
        {
            ObserveModifier(modifier, down);
            return true;
        }

        if (down)
        {
            pressedOtherKey = true;
            modifierOnly = null;
        }
        if (key is Keys.Tab or Keys.Escape or Keys.Enter)
            return false;

        if (down)
        {
            var binding = new ShortcutBinding(held, (uint)key);
            SelectBinding(binding.IsValid ? binding : null);
        }
        return true;
    }

    private void ObserveModifier(ShortcutModifiers modifier, bool down)
    {
        if (down)
        {
            if (held == ShortcutModifiers.None)
                ResetHeld();
            held |= modifier;
            var binding = new ShortcutBinding(held, 0);
            if (!pressedOtherKey && binding.IsValid)
                modifierOnly = binding;
            return;
        }

        held &= ~modifier;
        if (held == ShortcutModifiers.None)
        {
            if (!pressedOtherKey && modifierOnly is { } binding)
                SelectBinding(binding);
            ResetHeld();
        }
    }

    private void ResetHeld()
    {
        held = ShortcutModifiers.None;
        modifierOnly = null;
        pressedOtherKey = false;
    }

    private void SelectBinding(ShortcutBinding? binding)
    {
        SelectedBinding = binding;
        capture.Text = binding?.Label ?? "";
        useShortcut.Enabled = binding.HasValue;
        feedback.Text = binding.HasValue ? "" : "That shortcut is not supported.";
    }

    private static Label Help(string text) => new()
    {
        Text = text, AutoSize = true, MaximumSize = new Size(432, 0),
        Margin = new Padding(0, 0, 0, 4), UseMnemonic = false
    };

    protected override void Dispose(bool disposing)
    {
        if (disposing && filtering)
        {
            filtering = false;
            Application.RemoveMessageFilter(this);
        }
        base.Dispose(disposing);
    }
}
