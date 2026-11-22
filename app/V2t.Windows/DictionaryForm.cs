using V2t.Core;

namespace V2t.Windows;

internal sealed class DictionaryForm : Form
{
    private const int MaxEntries = 300;
    private const int MaxFieldLength = 160;

    private readonly DataGridView grid = new()
    {
        Dock = DockStyle.Fill,
        AllowUserToAddRows = false,
        AllowUserToDeleteRows = false,
        AllowUserToResizeRows = false,
        AutoSizeColumnsMode = DataGridViewAutoSizeColumnsMode.Fill,
        SelectionMode = DataGridViewSelectionMode.FullRowSelect,
        RowHeadersVisible = false,
        AccessibleName = "Dictionary entries"
    };
    private readonly Button add = new() { Text = "Add", AutoSize = true };
    private readonly Button remove = new() { Text = "Remove selected", AutoSize = true };
    private readonly Button installModel = new() { AutoSize = true };
    private readonly CheckBox useAi = new() { Text = "Use local AI to correct dictation", AutoSize = true };
    private readonly Label installationStatus = new() { AutoSize = true, MaximumSize = new Size(535, 0) };
    private readonly Label saveError = new() { AutoSize = true, MaximumSize = new Size(535, 0), Visible = false };
    private readonly Func<CorrectionPreferences, string?> save;

    public DictionaryForm(CorrectionPreferences preferences, Func<CorrectionPreferences, string?> save,
        Action install, bool installed, bool installing)
    {
        this.save = save;
        Text = "Dictionary & AI";
        Font = new Font("Segoe UI", 10);
        AutoScaleMode = AutoScaleMode.Font;
        StartPosition = FormStartPosition.CenterParent;
        ClientSize = new Size(595, 475);
        MinimumSize = new Size(540, 425);
        AppIcon.ApplyTo(this);
        ShowInTaskbar = false;

        grid.Columns.AddRange(TextColumn("From"), TextColumn("To"));
        foreach (var entry in preferences.Dictionary.Entries)
            grid.Rows.Add(entry.Spoken, entry.Replacement);
        useAi.Checked = preferences.EnableAi;

        var layout = new TableLayoutPanel
        {
            ColumnCount = 1, RowCount = 8, Dock = DockStyle.Fill, Padding = new Padding(18)
        };
        layout.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 100));
        layout.RowStyles.Add(new RowStyle(SizeType.AutoSize));
        layout.RowStyles.Add(new RowStyle(SizeType.Percent, 100));
        for (int row = 2; row < layout.RowCount; row++)
            layout.RowStyles.Add(new RowStyle(SizeType.AutoSize));

        var example = new Label { Text = "For example, replace โอเพนเราเตอร์ with OpenRouter.", AutoSize = true };
        var cancel = new Button { Text = "Cancel", AutoSize = true, DialogResult = DialogResult.Cancel };
        var saveButton = new Button { Text = "Save", AutoSize = true };
        var footer = Bar(saveButton, cancel);
        footer.FlowDirection = FlowDirection.RightToLeft;
        layout.Controls.Add(example, 0, 0);
        layout.Controls.Add(grid, 0, 1);
        layout.Controls.Add(Bar(add, remove), 0, 2);
        layout.Controls.Add(new Label { Text = "Dictionary entries apply with or without local AI.", AutoSize = true }, 0, 3);
        layout.Controls.Add(Bar(useAi, installModel), 0, 4);
        layout.Controls.Add(installationStatus, 0, 5);
        layout.Controls.Add(saveError, 0, 6);
        layout.Controls.Add(footer, 0, 7);
        Controls.Add(layout);

        AcceptButton = saveButton;
        CancelButton = cancel;
        add.Click += (_, _) => AddEntry();
        remove.Click += (_, _) => RemoveEntries();
        installModel.Click += (_, _) => install();
        saveButton.Click += (_, _) => SavePreferences();
        cancel.Click += (_, _) => Close();
        grid.SelectionChanged += (_, _) => UpdateEntryActions();
        UpdateEntryActions();
        UpdateInstallation(installed, installing);
    }

    public void UpdateInstallation(bool installed, bool installing, string? message = null)
    {
        installModel.Enabled = !installed && !installing;
        installModel.Text = installing ? "Installing local AI…" : installed ? "Local AI installed" : "Install local AI (2.3 GiB)";
        installationStatus.Text = message ?? "";
        installationStatus.Visible = !string.IsNullOrWhiteSpace(message);
    }

    private void AddEntry()
    {
        if (grid.Rows.Count >= MaxEntries || !grid.EndEdit())
            return;

        int index = grid.Rows.Add();
        grid.CurrentCell = grid.Rows[index].Cells[0];
        grid.Focus();
        grid.BeginEdit(selectAll: false);
        UpdateEntryActions();
    }

    private void RemoveEntries()
    {
        var selected = grid.SelectedRows.Cast<DataGridViewRow>().ToArray();
        grid.CancelEdit();
        foreach (var row in selected)
            grid.Rows.Remove(row);

        UpdateEntryActions();
    }

    private void UpdateEntryActions()
    {
        add.Enabled = grid.Rows.Count < MaxEntries;
        remove.Enabled = grid.SelectedRows.Count > 0;
    }

    private void SavePreferences()
    {
        if (!grid.EndEdit())
        {
            ShowSaveError("Finish editing the current entry and try again.");
            return;
        }

        try
        {
            var entries = grid.Rows.Cast<DataGridViewRow>().Select(row => new DictionaryEntry(
                row.Cells[0].Value as string ?? "", row.Cells[1].Value as string ?? ""));
            var result = save(new CorrectionPreferences(new CorrectionDictionary(entries), useAi.Checked));
            if (result is not null)
            {
                ShowSaveError(result);
                return;
            }
        }
        catch (ArgumentException error)
        {
            ShowSaveError(error.Message);
            return;
        }

        DialogResult = DialogResult.OK;
        Close();
    }

    private void ShowSaveError(string message)
    {
        saveError.Text = message;
        saveError.Visible = true;
    }

    private static DataGridViewTextBoxColumn TextColumn(string heading) => new()
    {
        HeaderText = heading, MaxInputLength = MaxFieldLength, SortMode = DataGridViewColumnSortMode.NotSortable
    };

    private static FlowLayoutPanel Bar(params Control[] children)
    {
        var panel = new FlowLayoutPanel { AutoSize = true, Dock = DockStyle.Fill, Margin = Padding.Empty };
        panel.Controls.AddRange(children);
        return panel;
    }
}
