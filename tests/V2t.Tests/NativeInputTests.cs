using System.Runtime.InteropServices;
using V2t.Windows;

namespace V2t.Tests;

internal static class NativeInputTests
{
    public static void Run()
    {
        var input = WindowsInput.CreateUnicodeInput("ก🌏");
        AssertEx.True(input.Length == 6 && Marshal.SizeOf(input[0]) == (IntPtr.Size == 8 ? 40 : 28));
        AssertEx.True(input[0].Value.Keyboard.ScanCode == 'ก' && input[0].Value.Keyboard.Flags == 4);
        AssertEx.True(input[1].Value.Keyboard.ScanCode == 'ก' && input[1].Value.Keyboard.Flags == 6);
        AssertEx.True(char.IsHighSurrogate((char)input[2].Value.Keyboard.ScanCode) && char.IsLowSurrogate((char)input[4].Value.Keyboard.ScanCode));
        AssertEx.True(new FocusTarget(2, 3).Matches(new FocusTarget(2, 3)));
        AssertEx.True(!new FocusTarget(2, 3).Matches(new FocusTarget(2, 4)));
        AssertEx.True(!new FocusTarget(2, 3).Matches(new FocusTarget(4, 3)));
        AssertEx.True(new FocusTarget(2, 0).Matches(new FocusTarget(2, 3)));
        AssertEx.True(!default(FocusTarget).Matches(default));

        using var audio = new PcmWaveCapture();
        AssertEx.True(!audio.Append([1, 2, 3, 4]));
        AssertEx.True(!audio.Append([5, 6]));
        var wav = audio.Finish();
        using var reader = new BinaryReader(new MemoryStream(wav));
        AssertEx.True(reader.ReadUInt32() == 0x46464952);
        AssertEx.True(reader.ReadUInt32() == wav.Length - 8);
        reader.BaseStream.Position = 22;
        AssertEx.True(reader.ReadUInt16() == 1 && reader.ReadUInt32() == 16000);
        reader.BaseStream.Position = 34;
        AssertEx.True(reader.ReadUInt16() == 16);
        reader.BaseStream.Position = 40;
        AssertEx.True(reader.ReadUInt32() == 6 && reader.ReadBytes(6).SequenceEqual(new byte[] { 1, 2, 3, 4, 5, 6 }));
        using var max = new PcmWaveCapture();
        AssertEx.True(max.Append(new byte[10_000_000]));
        AssertEx.True(max.Finish().Length == 9_600_044);
        Console.WriteLine("Native I/O invariants passed.");
    }
}
