using V2t.Core;

namespace V2t.Tests;

internal static class CorrectionDictionaryTests
{
    internal static void Run()
    {
        UsesLongestMatchWithoutCascading();
        MatchesOnlyWholeEnglishWords();
        MatchesTechnicalTermsWithPunctuation();
        MatchesThaiWithinContinuousText();
        ReportsDistinctTermsInApplicationOrder();
        SupportsEmptyDictionaryAndEmptyInput();
        CopiesAndProtectsEntries();
        ValidatesEntries();
        Console.WriteLine("CorrectionDictionary: 8 tests passed");
    }

    private static void UsesLongestMatchWithoutCascading()
    {
        var dictionary = new CorrectionDictionary([
            new("เอไอ", "AI"), new("โอเพนเอไอ", "OpenAI"),
            new("โอเพน", "Open"), new("OpenAI", "unexpected")
        ]);

        var result = dictionary.Apply("ลองโอเพนเอไอและเอไอ แล้วใช้ OpenAI");

        AssertEx.Equal("ลองOpenAIและAI แล้วใช้ unexpected", result.Text);
        AssertEx.True(result.AppliedTerms.SequenceEqual(["OpenAI", "AI", "unexpected"]));
    }

    private static void MatchesOnlyWholeEnglishWords()
    {
        var dictionary = new CorrectionDictionary([new("ai", "AI")]);

        var result = dictionary.Apply("aI (ai) AI2 xai ai_name _AI AI_2 ai。 เจอaiแล้ว");

        AssertEx.Equal("AI (AI) AI2 xai ai_name _AI AI_2 AI。 เจอAIแล้ว", result.Text);
    }

    private static void MatchesTechnicalTermsWithPunctuation()
    {
        var dictionary = new CorrectionDictionary([new("c++", "C++"), new(".net", ".NET")]);

        var result = dictionary.Apply("c++ c++Builder .net website.net กับc++แล้ว");

        AssertEx.Equal("C++ c++Builder .NET website.net กับC++แล้ว", result.Text);
    }

    private static void MatchesThaiWithinContinuousText()
    {
        var dictionary = new CorrectionDictionary([new("กิตฮับ", "GitHub"), new("ใช้ API", "ใช้ interface")]);

        var result = dictionary.Apply("ไปกิตฮับแล้วใช้ apiได้ แต่ใช้ API2 อยู่");

        AssertEx.Equal("ไปGitHubแล้วใช้ interfaceได้ แต่ใช้ API2 อยู่", result.Text);
    }

    private static void ReportsDistinctTermsInApplicationOrder()
    {
        var dictionary = new CorrectionDictionary([
            new("ซิกเนล", "SigNal"), new("สัญญาณ", "SigNal"), new("โอเพนเอไอ", "OpenAI")
        ]);

        var result = dictionary.Apply("โอเพนเอไอ ซิกเนล สัญญาณ ซิกเนล");

        AssertEx.Equal("OpenAI SigNal SigNal SigNal", result.Text);
        AssertEx.True(result.AppliedTerms.SequenceEqual(["OpenAI", "SigNal"]));
    }

    private static void SupportsEmptyDictionaryAndEmptyInput()
    {
        AssertEx.Equal(" Keep original spaces 🙂 ", new CorrectionDictionary([]).Apply(" Keep original spaces 🙂 ").Text);
        var dictionary = new CorrectionDictionary([new("ai", "AI")]);
        AssertEx.Equal("", dictionary.Apply("").Text);
        AssertEx.True(dictionary.Apply("Nothing here").AppliedTerms.Count == 0);
    }

    private static void CopiesAndProtectsEntries()
    {
        var input = new List<DictionaryEntry> { new("ai", "AI") };
        var dictionary = new CorrectionDictionary(input);
        input[0] = new("ai", "wrong");

        AssertEx.Equal(new DictionaryEntry("ai", "AI"), dictionary.Entries[0]);
        if (dictionary.Entries is IList<DictionaryEntry> list)
            AssertEx.True(list.IsReadOnly);
        AssertEx.Equal("AI", dictionary.Apply("ai").Text);
    }

    private static void ValidatesEntries()
    {
        Rejects([new("", "valid")], "spoken");
        Rejects([new("valid", "   ")], "replacement");
        Rejects([new("foo\tbar", "valid")], "control");
        Rejects([new("valid", "foo\nbar")], "control");
        Rejects([new(new string('x', 161), "valid")], "160");
        Rejects([new("valid", new string('x', 161))], "160");
        Rejects([new("AI", "a"), new("ai", "b")], "duplicate");
        Rejects([null!], "entry");
        Rejects(Enumerable.Range(0, 301).Select(index => new DictionaryEntry($"term {index}", "term")), "300");

        var max = new CorrectionDictionary(Enumerable.Range(0, 300).Select(index =>
            new DictionaryEntry($"{new string('x', 157)}{index:000}", new string('Y', 160))));
        AssertEx.Equal(300, max.Entries.Count);
        AssertEx.Equal(new string('Y', 160), max.Apply($"{new string('x', 157)}299").Text);
    }

    private static void Rejects(IEnumerable<DictionaryEntry> entries, string reason)
    {
        try
        {
            _ = new CorrectionDictionary(entries);
        }
        catch (ArgumentException error)
        {
            AssertEx.True(error.Message.Contains(reason, StringComparison.OrdinalIgnoreCase), error.Message);
            return;
        }

        throw new InvalidOperationException($"Expected dictionary validation to reject {reason}.");
    }
}
