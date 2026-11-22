using System.Text;

namespace V2t.Core;

public sealed record DictionaryEntry(string Spoken, string Replacement);

public sealed record DictionaryMatch(string Text, IReadOnlyList<string> AppliedTerms);

public sealed class CorrectionDictionary
{
    private const int MaxEntries = 300;
    private const int MaxFieldLength = 160;
    private readonly ILookup<char, Rule> rules;

    public CorrectionDictionary(IEnumerable<DictionaryEntry> entries)
    {
        ArgumentNullException.ThrowIfNull(entries);
        var validated = new List<DictionaryEntry>();
        var spokenTerms = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        foreach (var entry in entries)
        {
            if (validated.Count == MaxEntries)
                throw new ArgumentException($"A dictionary can have at most {MaxEntries} entries.", nameof(entries));

            var number = validated.Count + 1;
            if (entry is null)
                throw new ArgumentException($"Dictionary entry {number} is missing.", nameof(entries));

            ValidateField(entry.Spoken, "spoken", number);
            ValidateField(entry.Replacement, "replacement", number);
            if (!spokenTerms.Add(entry.Spoken))
                throw new ArgumentException($"Dictionary entry {number} has a duplicate spoken term.", nameof(entries));

            validated.Add(entry);
        }

        Entries = validated.AsReadOnly();
        rules = validated.OrderByDescending(entry => entry.Spoken.Length)
            .Select(entry => new Rule(entry,
                RequiresEnglishBoundary(entry.Spoken, start: true),
                RequiresEnglishBoundary(entry.Spoken, start: false)))
            .ToLookup(rule => char.ToUpperInvariant(rule.Entry.Spoken[0]));
    }

    public IReadOnlyList<DictionaryEntry> Entries { get; }

    public DictionaryMatch Apply(string transcript)
    {
        ArgumentNullException.ThrowIfNull(transcript);
        if (Entries.Count == 0 || transcript.Length == 0)
            return new DictionaryMatch(transcript, []);

        StringBuilder? output = null;
        var applied = new List<string>();
        var seen = new HashSet<string>(StringComparer.Ordinal);
        int unchangedStart = 0;
        for (int offset = 0; offset < transcript.Length;)
        {
            var match = FindMatch(transcript, offset);
            if (match is null)
            {
                offset++;
                continue;
            }

            output ??= new StringBuilder(transcript.Length);
            output.Append(transcript.AsSpan(unchangedStart, offset - unchangedStart));
            output.Append(match.Replacement);
            if (seen.Add(match.Replacement))
                applied.Add(match.Replacement);

            offset += match.Spoken.Length;
            unchangedStart = offset;
        }

        if (output is null)
            return new DictionaryMatch(transcript, []);

        output.Append(transcript.AsSpan(unchangedStart));
        return new DictionaryMatch(output.ToString(), applied.AsReadOnly());
    }

    private DictionaryEntry? FindMatch(string transcript, int offset)
    {
        var remainder = transcript.AsSpan(offset);
        foreach (var rule in rules[char.ToUpperInvariant(remainder[0])])
        {
            var spoken = rule.Entry.Spoken;
            if (!remainder.StartsWith(spoken, StringComparison.OrdinalIgnoreCase))
                continue;
            if (rule.LeftBoundary && offset > 0 && IsEnglishWordCharacter(transcript[offset - 1]))
                continue;
            if (rule.RightBoundary && remainder.Length > spoken.Length && IsEnglishWordCharacter(remainder[spoken.Length]))
                continue;

            return rule.Entry;
        }

        return null;
    }

    private static bool RequiresEnglishBoundary(string spoken, bool start)
    {
        // Skip punctuation next to the word so terms such as C++ and .NET stay whole.
        for (int index = 0; index < spoken.Length; index++)
        {
            char character = spoken[start ? index : spoken.Length - index - 1];
            if (char.IsWhiteSpace(character))
                return false;
            if (char.IsLetterOrDigit(character) || character == '_')
                return IsEnglishWordCharacter(character);
        }

        return false;
    }

    private static bool IsEnglishWordCharacter(char character) => char.IsAsciiLetterOrDigit(character) || character == '_';

    private static void ValidateField(string field, string name, int number)
    {
        if (string.IsNullOrWhiteSpace(field))
            throw new ArgumentException($"The {name} text for dictionary entry {number} is required.", "entries");
        if (field.Length > MaxFieldLength)
            throw new ArgumentException($"The {name} text for dictionary entry {number} can have at most {MaxFieldLength} characters.", "entries");
        if (field.Any(char.IsControl))
            throw new ArgumentException($"The {name} text for dictionary entry {number} cannot contain control characters.", "entries");
    }

    private readonly record struct Rule(DictionaryEntry Entry, bool LeftBoundary, bool RightBoundary);
}
