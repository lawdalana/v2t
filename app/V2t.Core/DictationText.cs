using System.Text;

namespace V2t.Core;

public static class DictationText
{
    public static string ForInsertion(string transcript)
    {
        var text = new StringBuilder(transcript.Length);
        var needsSpace = false;
        foreach (var character in transcript)
        {
            if (char.IsWhiteSpace(character))
            {
                needsSpace = text.Length > 0;
            }
            else if (!char.IsControl(character))
            {
                if (needsSpace)
                    text.Append(' ');
                text.Append(character);
                needsSpace = false;
            }
        }
        return text.ToString();
    }
}
