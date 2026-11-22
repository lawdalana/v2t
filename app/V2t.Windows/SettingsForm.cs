using System.Windows.Forms.Automation;
using V2t.Core;

namespace V2t.Windows;

internal sealed class SettingsForm : Form
{
    private static readonly (string Caption, string? Code)[] LanguageOptions =
    [
        ("Thai + English (mixed)", TranscriptionClient.MixedThaiEnglish),
        ("Detect automatically (all languages)", null),
        ("Thai", "th"),
        ("English", "en")
    ];

    private static Color Canvas => SystemInformation.HighContrast ? SystemColors.Window : Color.FromArgb(244, 245, 249);
    private static Color Surface => SystemInformation.HighContrast ? SystemColors.Window : Color.White;
    private static Color Ink => SystemInformation.HighContrast ? SystemColors.WindowText : Color.FromArgb(30, 30, 47);
    private static Color Muted => SystemInformation.HighContrast ? SystemColors.WindowText : Color.FromArgb(82, 88, 111);
    private static Color Line => SystemInformation.HighContrast ? SystemColors.WindowText : Color.FromArgb(216, 219, 231);
    private static Color Accent => SystemInformation.HighContrast ? SystemColors.Highlight : Color.FromArgb(75, 49, 150);

    private readonly ComboBox provider = new() { DropDownStyle = ComboBoxStyle.DropDownList };
    private readonly ComboBox language = new() { DropDownStyle = ComboBoxStyle.DropDownList };
    private readonly Label languageHelp = new();
    private readonly Label savedShortcut = TextLabel("");
    private readonly Label shortcutLabel = TextLabel("");
    private readonly TextBox openAiKey = new() { UseSystemPasswordChar = true, MaxLength = 512 };
    private readonly TextBox openRouterKey = new() { UseSystemPasswordChar = true, MaxLength = 512 };
    private readonly CheckBox maiBackup = new() { Text = "Use &MAI-Transcribe-2 as backup" };
    private readonly Button localSetup = new() { Text = "Setup", AccessibleName = "Setup local speech model", AutoSize = true };
    private readonly Label status = new() { LiveSetting = AutomationLiveSetting.Polite };
    private readonly Font bodyFont = new("Segoe UI", 10);
    private readonly Font headingFont;
    private readonly Font sectionFont;
    private readonly Font emphasisFont;
    private readonly Func<DictationSettings, string?> apply;
    private readonly Action<bool> commitOpenAiKey;
    private readonly Action<bool> commitOpenRouterKey;
    private DictationSettings applied;
    private ShortcutCaptureDialog? shortcutDialog;
    private bool allowClose;
    private bool installing;
    private bool refreshingChoices;
    private bool applying;
    private string? SelectedLanguage => language.SelectedIndex >= 0
        ? LanguageOptions[language.SelectedIndex].Code : applied.Language;

    public SettingsForm(DictationSettings settings, Func<DictationSettings, string?> apply)
    {
        this.apply = apply;
        applied = settings;
        Font = bodyFont;
        headingFont = new Font(Font.FontFamily, 24, FontStyle.Bold);
        sectionFont = new Font(Font.FontFamily, 13, FontStyle.Bold);
        emphasisFont = new Font(Font, FontStyle.Bold);
        SuspendLayout();
        AutoScaleDimensions = new SizeF(96, 96);
        AutoScaleMode = AutoScaleMode.Dpi;
        Text = "v2t Settings";
        StartPosition = FormStartPosition.CenterScreen;
        MinimizeBox = false;
        ClientSize = new Size(884, 574);
        AppIcon.ApplyTo(this);
        BackColor = Canvas;
        ForeColor = Ink;

        provider.Items.AddRange(["Local · Whisper Turbo", "OpenAI · GPT Transcribe", "OpenRouter · Whisper Turbo"]);
        language.Items.AddRange(LanguageOptions.Select(option => option.Caption).ToArray());
        UpdateShortcut(settings.Hotkey);

        var dictionary = new Button { Text = "&Dictionary && AI…", AutoSize = true };
        Controls.Add(BuildLayout(dictionary));

        RefreshChoices();
        provider.SelectedIndexChanged += (_, _) =>
            ApplyChoice(applied with { Provider = (SpeechProvider)provider.SelectedIndex });
        language.SelectedIndexChanged += (_, _) => ApplyChoice(applied with { Language = SelectedLanguage });
        maiBackup.CheckedChanged += (_, _) => ApplyChoice(applied with { EnableMaiBackup = maiBackup.Checked });
        openAiKey.Text = settings.OpenAiKey;
        openRouterKey.Text = settings.OpenRouterKey;
        commitOpenAiKey = BindApiKey(openAiKey, value => value.OpenAiKey,
            (value, key) => value with { OpenAiKey = key });
        commitOpenRouterKey = BindApiKey(openRouterKey, value => value.OpenRouterKey,
            (value, key) => value with { OpenRouterKey = key });
        SetInstalling(false);
        localSetup.Click += (_, _) => InstallRequested?.Invoke(SelectedLanguage);
        dictionary.Click += (_, _) => DictionaryRequested?.Invoke();
        Deactivate += (_, _) => CommitPendingKeys();
        FormClosing += (_, e) =>
        {
            if (!allowClose && e.CloseReason == CloseReason.UserClosing)
            {
                CommitPendingKeys();
                e.Cancel = true;
                Hide();
            }
        };
        ResumeLayout(performLayout: true);
    }

    private TableLayoutPanel BuildLayout(Button dictionary)
    {
        var layout = new TableLayoutPanel
        {
            ColumnCount = 1, RowCount = 3, Dock = DockStyle.Fill
        };
        layout.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 100));
        layout.RowStyles.Add(new RowStyle(SizeType.AutoSize));
        layout.RowStyles.Add(new RowStyle(SizeType.Percent, 100));
        layout.RowStyles.Add(new RowStyle(SizeType.AutoSize));
        layout.Controls.Add(BuildHeader(), 0, 0);
        layout.Controls.Add(BuildSections(), 0, 1);
        layout.Controls.Add(BuildFooter(dictionary), 0, 2);
        return layout;
    }

    private TableLayoutPanel BuildHeader()
    {
        var header = Band(2, new Padding(24, 18, 24, 18));
        var title = TextLabel("Voice typing", headingFont);
        title.Anchor = AnchorStyles.Left;
        header.Controls.Add(title, 0, 0);

        savedShortcut.Font = emphasisFont;
        savedShortcut.BackColor = SystemInformation.HighContrast ? SystemColors.Highlight : Color.FromArgb(237, 231, 252);
        savedShortcut.ForeColor = SystemInformation.HighContrast ? SystemColors.HighlightText : Accent;
        savedShortcut.Padding = new Padding(10, 6, 10, 6);
        savedShortcut.Anchor = AnchorStyles.Left;
        var guide = TextLabel("Hold to speak.\nRelease to type.");
        guide.ForeColor = Muted;
        guide.Margin = new Padding(10, 0, 0, 0);
        guide.Anchor = AnchorStyles.Left;
        var hint = new TableLayoutPanel { ColumnCount = 2, AutoSize = true, Margin = Padding.Empty };
        hint.Controls.Add(savedShortcut, 0, 0);
        hint.Controls.Add(guide, 1, 0);
        header.Controls.Add(hint, 1, 0);
        header.Paint += (_, e) => ControlPaint.DrawBorder(e.Graphics, header.ClientRectangle,
            Color.Empty, 0, ButtonBorderStyle.None, Color.Empty, 0, ButtonBorderStyle.None,
            Color.Empty, 0, ButtonBorderStyle.None, Line, 1, ButtonBorderStyle.Solid);
        return header;
    }

    private Panel BuildSections()
    {
        var viewport = new Panel { Dock = DockStyle.Fill, AutoScroll = true, Margin = Padding.Empty, TabIndex = 1 };
        var columns = new TableLayoutPanel
        {
            ColumnCount = 2, RowCount = 2, Dock = DockStyle.Top, AutoSize = true,
            AutoSizeMode = AutoSizeMode.GrowAndShrink, Padding = new Padding(24, 20, 24, 16)
        };
        columns.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 50));
        columns.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 50));
        columns.RowStyles.Add(new RowStyle(SizeType.AutoSize));
        columns.RowStyles.Add(new RowStyle(SizeType.AutoSize));

        var speech = BuildSpeechCard();
        var cloud = BuildCloudCard();
        cloud.TabIndex = 1;
        columns.Controls.Add(speech, 0, 0);
        columns.Controls.Add(cloud, 1, 0);
        void UpdateScrollRange(object? sender, EventArgs e)
        {
            if (!viewport.IsHandleCreated)
                return;
            // Update after dock layout finishes so WinForms uses the reflowed table height.
            viewport.BeginInvoke(() =>
            {
                if (viewport.AutoScrollMinSize.Height != columns.Height)
                    viewport.AutoScrollMinSize = new Size(0, columns.Height);
            });
        }
        columns.SizeChanged += UpdateScrollRange;
        viewport.HandleCreated += UpdateScrollRange;
        viewport.Controls.Add(columns);

        bool stacked = false;
        viewport.ClientSizeChanged += (_, _) =>
        {
            bool needsStack = viewport.ClientSize.Width < LogicalToDeviceUnits(690);
            if (needsStack == stacked)
                return;
            stacked = needsStack;
            columns.SuspendLayout();
            if (stacked)
                columns.SetCellPosition(cloud, new TableLayoutPanelCellPosition(0, 1));
            columns.ColumnCount = stacked ? 1 : 2;
            if (!stacked)
                columns.SetCellPosition(cloud, new TableLayoutPanelCellPosition(1, 0));
            speech.Margin = stacked ? new Padding(0, 0, 0, LogicalToDeviceUnits(16))
                : new Padding(0, 0, LogicalToDeviceUnits(8), 0);
            cloud.Margin = stacked ? Padding.Empty : new Padding(LogicalToDeviceUnits(8), 0, 0, 0);
            columns.ResumeLayout(performLayout: true);
        };
        return viewport;
    }

    private TableLayoutPanel BuildSpeechCard()
    {
        var card = Card("Transcription");
        card.Margin = new Padding(0, 0, 8, 0);
        AddField(card, "Speech &provider", provider);
        AddField(card, "&Language", language);
        ConfigureHelp(languageHelp);
        AddCardRow(card, languageHelp);
        AddCardRow(card, TextLabel("Dictation shortcut", emphasisFont));
        AddCardRow(card, BuildShortcutControl());
        return card;
    }

    private TableLayoutPanel BuildShortcutControl()
    {
        var field = new TableLayoutPanel
        {
            ColumnCount = 2, RowCount = 1, AutoSize = true, AutoSizeMode = AutoSizeMode.GrowAndShrink,
            Dock = DockStyle.Top, Margin = new Padding(0, 4, 0, 8)
        };
        field.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 100));
        field.ColumnStyles.Add(new ColumnStyle(SizeType.AutoSize));
        shortcutLabel.Anchor = AnchorStyles.Left;
        field.Controls.Add(shortcutLabel, 0, 0);

        var change = new Button { Text = "&Change…", AccessibleName = "Change dictation shortcut", AutoSize = true };
        StyleButton(change);
        change.Click += (_, _) => ChangeShortcut();
        field.Controls.Add(change, 1, 0);
        return field;
    }

    private TableLayoutPanel BuildCloudCard()
    {
        var card = Card("API & backup");
        card.Margin = new Padding(8, 0, 0, 0);
        AddField(card, "&OpenAI API key", openAiKey);
        AddField(card, "Open&Router API key", openRouterKey);
        maiBackup.Dock = DockStyle.Top;
        maiBackup.Height = 32;
        maiBackup.Margin = new Padding(0, 4, 0, 0);
        AddCardRow(card, maiBackup);
        var backupHelp = new Label { Text = "Via OpenRouter when local transcription is busy or fails." };
        ConfigureHelp(backupHelp);
        backupHelp.Margin = new Padding(22, 0, 0, 0);
        AddCardRow(card, backupHelp);
        return card;
    }

    private TableLayoutPanel BuildFooter(Button dictionary)
    {
        var footer = Band(3, new Padding(24, 14, 24, 14));
        footer.TabIndex = 2;
        var statusRow = new RowStyle(SizeType.Absolute, 48);
        footer.RowStyles.Add(statusRow);
        status.Dock = DockStyle.Fill;
        status.ForeColor = Muted;
        status.Margin = new Padding(0, 0, 16, 0);
        status.TextAlign = ContentAlignment.MiddleLeft;
        status.UseMnemonic = false;
        status.SizeChanged += (_, _) => UpdateStatusHeight(statusRow);
        status.TextChanged += (_, _) => UpdateStatusHeight(statusRow);
        status.HandleCreated += (_, _) => UpdateStatusHeight(statusRow);
        footer.Controls.Add(status, 0, 0);
        StyleButton(dictionary);
        dictionary.Margin = new Padding(0, 0, 8, 0);
        footer.Controls.Add(dictionary, 1, 0);
        StyleButton(localSetup);
        footer.Controls.Add(localSetup, 2, 0);
        footer.Paint += (_, e) => ControlPaint.DrawBorder(e.Graphics, footer.ClientRectangle,
            Color.Empty, 0, ButtonBorderStyle.None, Line, 1, ButtonBorderStyle.Solid,
            Color.Empty, 0, ButtonBorderStyle.None, Color.Empty, 0, ButtonBorderStyle.None);
        return footer;
    }

    private void UpdateStatusHeight(RowStyle row)
    {
        if (!status.IsHandleCreated || status.Width == 0)
            return;
        status.BeginInvoke(() =>
        {
            var available = new Size(status.Width, int.MaxValue);
            int textHeight = TextRenderer.MeasureText(status.Text, status.Font, available,
                TextFormatFlags.WordBreak | TextFormatFlags.TextBoxControl | TextFormatFlags.NoPrefix).Height;
            int height = Math.Max(LogicalToDeviceUnits(48), textHeight);
            if (row.Height != height)
                row.Height = height;
        });
    }

    public event Action<string?>? InstallRequested;
    public event Action? DictionaryRequested;

    public void SetStatus(string message) => status.Text = message;

    internal bool OwnsShortcutTarget(nint window) => window != 0 &&
        ((IsHandleCreated && window == Handle) ||
         (shortcutDialog is { IsHandleCreated: true } editor && window == editor.Handle));

    private void UpdateShortcut(ShortcutBinding binding)
    {
        savedShortcut.Text = binding.Label.Replace("+", " + ");
        shortcutLabel.Text = binding.Label;
    }

    private void ApplyChoice(DictationSettings updated)
    {
        if (refreshingChoices || applying || allowClose)
            return;

        var previousLanguage = applied.Language;
        TryApply(updated);
        RefreshChoices();
        if (previousLanguage != applied.Language)
            SetInstalling(installing);
    }

    private void RefreshChoices()
    {
        refreshingChoices = true;
        try
        {
            provider.SelectedIndex = (int)applied.Provider;
            language.SelectedIndex = Array.FindIndex(LanguageOptions, option => option.Code == applied.Language);
            maiBackup.Checked = applied.EnableMaiBackup;
            maiBackup.Enabled = applied.Provider == SpeechProvider.Local;
            UpdateLanguageHelp();
        }
        finally
        {
            refreshingChoices = false;
        }
    }

    private bool TryApply(DictationSettings updated)
    {
        if (applying || allowClose)
            return false;
        if (updated == applied)
            return true;

        applying = true;
        try
        {
            if (apply(updated) is { } error)
            {
                SetStatus(error);
                return false;
            }
            applied = updated;
            return true;
        }
        finally
        {
            applying = false;
        }
    }

    private Action<bool> BindApiKey(TextBox field, Func<DictationSettings, string> get,
        Func<DictationSettings, string, DictationSettings> update)
    {
        bool edited = false;
        field.TextChanged += (_, _) => edited = true;
        field.Leave += (_, _) => Commit(retry: false);
        return Commit;

        void Commit(bool retry)
        {
            if (allowClose || applying || (!edited && !retry))
                return;

            string key = field.Text.Trim();
            if (key == get(applied) || TryApply(update(applied, key)))
                field.Text = key;
            edited = false;
        }
    }

    private void CommitPendingKeys()
    {
        commitOpenAiKey(false);
        commitOpenRouterKey(false);
    }

    protected override bool ProcessCmdKey(ref Message msg, Keys keyData)
    {
        if ((keyData & Keys.KeyCode) == Keys.Enter)
        {
            if (openAiKey.Focused)
            {
                commitOpenAiKey(true);
                return true;
            }
            if (openRouterKey.Focused)
            {
                commitOpenRouterKey(true);
                return true;
            }
        }
        return base.ProcessCmdKey(ref msg, keyData);
    }

    private void ChangeShortcut()
    {
        using var dialog = new ShortcutCaptureDialog(applied.Hotkey);
        shortcutDialog = dialog;
        try
        {
            if (dialog.ShowDialog(this) == DialogResult.OK && dialog.SelectedBinding is { } selected &&
                selected != applied.Hotkey && TryApply(applied with { Shortcut = selected }))
                UpdateShortcut(applied.Hotkey);
        }
        finally
        {
            shortcutDialog = null;
        }
    }

    private void UpdateLanguageHelp()
    {
        languageHelp.Text = language.SelectedIndex == 0 ? (SpeechProvider)provider.SelectedIndex switch
        {
            SpeechProvider.Local => "Typhoon Whisper Turbo is preferred for Thai and mixed speech.",
            SpeechProvider.OpenAI => "OpenAI receives Thai and English as language hints.",
            _ => "OpenRouter uses Whisper’s automatic language detection."
        } : "";
    }

    public void SetInstalling(bool isInstalling)
    {
        installing = isInstalling;
        var installed = LocalEngine.PreferredInstalled(SelectedLanguage);
        localSetup.Enabled = !isInstalling && !installed;
        if (!SystemInformation.HighContrast)
            localSetup.BackColor = localSetup.Enabled ? Surface : SystemColors.Control;
        localSetup.AccessibleDescription = isInstalling ? "Local model setup is in progress."
            : installed ? "The local speech model is installed." : "Install the local speech model.";
    }

    public void CloseForExit()
    {
        allowClose = true;
        Close();
    }

    protected override void OnLoad(EventArgs e)
    {
        var area = Screen.FromControl(this).WorkingArea;
        int width = Math.Min(Width, area.Width);
        int height = Math.Min(Height, area.Height);
        MinimumSize = new Size(Math.Min(LogicalToDeviceUnits(568), width), Math.Min(LogicalToDeviceUnits(390), height));
        if (width != Width || height != Height)
            Bounds = new Rectangle(area.Left + (area.Width - width) / 2, area.Top + (area.Height - height) / 2, width, height);
        base.OnLoad(e);
    }

    protected override void Dispose(bool disposing)
    {
        base.Dispose(disposing);
        if (disposing)
        {
            headingFont.Dispose();
            sectionFont.Dispose();
            emphasisFont.Dispose();
            bodyFont.Dispose();
        }
    }

    private static TableLayoutPanel Band(int columnCount, Padding padding)
    {
        var band = new TableLayoutPanel
        {
            ColumnCount = columnCount, RowCount = 1, AutoSize = true, AutoSizeMode = AutoSizeMode.GrowAndShrink,
            Dock = DockStyle.Fill, BackColor = Surface, Margin = Padding.Empty, Padding = padding
        };
        band.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 100));
        for (int index = 1; index < columnCount; index++)
            band.ColumnStyles.Add(new ColumnStyle(SizeType.AutoSize));
        return band;
    }

    private TableLayoutPanel Card(string caption)
    {
        var card = new TableLayoutPanel
        {
            ColumnCount = 1, AutoSize = true, AutoSizeMode = AutoSizeMode.GrowAndShrink,
            Dock = DockStyle.Fill, BackColor = Surface, Padding = new Padding(18),
            AccessibleRole = AccessibleRole.Grouping, AccessibleName = caption
        };
        card.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 100));
        card.Paint += (_, e) => ControlPaint.DrawBorder(e.Graphics, card.ClientRectangle, Line, ButtonBorderStyle.Solid);
        var title = TextLabel(caption, sectionFont);
        title.UseMnemonic = false;
        title.Margin = new Padding(0, 0, 0, 14);
        AddCardRow(card, title);
        return card;
    }

    private void AddField(TableLayoutPanel card, string caption, Control input)
    {
        var label = TextLabel(caption, emphasisFont);
        label.Margin = new Padding(0, 0, 0, 5);
        AddCardRow(card, label);
        input.AccessibleName = caption.Replace("&", "");
        input.Margin = new Padding(0, 0, 0, 12);
        input.Dock = DockStyle.Top;
        AddCardRow(card, input);
    }

    private static void AddCardRow(TableLayoutPanel card, Control element)
    {
        int row = card.RowCount++;
        element.TabIndex = row;
        card.RowStyles.Add(new RowStyle(SizeType.AutoSize));
        card.Controls.Add(element, 0, row);
    }

    private static Label TextLabel(string text, Font? font = null)
    {
        var label = new Label { Text = text, AutoSize = true, Margin = Padding.Empty };
        if (font is not null)
            label.Font = font;
        return label;
    }

    private static void ConfigureHelp(Label label)
    {
        label.Dock = DockStyle.Top;
        label.Height = 40;
        label.ForeColor = Muted;
        label.Margin = Padding.Empty;
        label.UseMnemonic = false;
    }

    private void StyleButton(Button button)
    {
        button.Anchor = AnchorStyles.Left;
        button.AutoSizeMode = AutoSizeMode.GrowAndShrink;
        button.MinimumSize = new Size(0, 40);
        button.Padding = new Padding(12, 5, 12, 5);
        button.Margin = Padding.Empty;
        if (SystemInformation.HighContrast)
            return;
        button.FlatStyle = FlatStyle.Flat;
        button.FlatAppearance.BorderColor = Line;
        button.FlatAppearance.MouseOverBackColor = Canvas;
        button.FlatAppearance.MouseDownBackColor = Color.FromArgb(230, 232, 242);
        button.BackColor = Surface;
        button.ForeColor = Ink;
        button.Font = emphasisFont;
    }
}
