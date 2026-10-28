using System.Globalization;
using System.Text;

namespace LowPolyMap.Diagnostics;

/// <summary>JSON text that is written verbatim as an object field (an already serialised object).</summary>
public readonly record struct RawJson(string Text)
{
    public override string ToString() => Text;
}

/// <summary>
/// A small forward-only JSON writer for the benchmark result. Numbers are invariant culture; NaN and infinities, which JSON
/// cannot hold, are written as the strings "NaN", "Infinity", "-Infinity". Output is one line; <see cref="Pretty"/> indents it.
/// </summary>
public sealed class JsonText
{
    readonly StringBuilder _sb = new(256);
    bool _needComma;

    public void Begin() { Comma(); _sb.Append('{'); _needComma = false; }
    public void End() { _sb.Append('}'); _needComma = true; }

    /// <summary>Starts a nested object as the value of <paramref name="key"/>.</summary>
    public void BeginObject(string key) { Key(key); _sb.Append('{'); _needComma = false; }

    public void Field(string key, string value) { Key(key); String(_sb, value); }
    public void Field(string key, bool value) { Key(key); _sb.Append(value ? "true" : "false"); }
    public void Field(string key, long value) { Key(key); _sb.Append(value.ToString(CultureInfo.InvariantCulture)); }
    public void Field(string key, double value) { Key(key); Number(_sb, value); }
    public void Field(string key, RawJson value) { Key(key); _sb.Append(string.IsNullOrEmpty(value.Text) ? "null" : value.Text); }

    public override string ToString() => _sb.ToString();

    /// <summary>The text with one key per line and two-space indents (strings are not touched).</summary>
    public string Pretty()
    {
        var o = new StringBuilder(_sb.Length * 2);
        int depth = 0;
        bool inString = false;
        for (int i = 0; i < _sb.Length; i++)
        {
            char c = _sb[i];
            if (inString)
            {
                o.Append(c);
                if (c == '\\') o.Append(_sb[++i]);
                else if (c == '"') inString = false;
                continue;
            }
            switch (c)
            {
                case '"': inString = true; o.Append(c); break;
                case '{': o.Append(c).Append('\n').Append(' ', ++depth * 2); break;
                case '}': o.Append('\n').Append(' ', --depth * 2).Append(c); break;
                case ',': o.Append(c).Append('\n').Append(' ', depth * 2); break;
                case ':': o.Append(": "); break;
                default: o.Append(c); break;
            }
        }
        return o.ToString();
    }

    void Key(string key)
    {
        Comma();
        String(_sb, key);
        _sb.Append(':');
        _needComma = true;
    }

    void Comma()
    {
        if (_needComma) _sb.Append(',');
    }

    static void Number(StringBuilder sb, double d)
    {
        if (double.IsNaN(d)) sb.Append("\"NaN\"");
        else if (double.IsPositiveInfinity(d)) sb.Append("\"Infinity\"");
        else if (double.IsNegativeInfinity(d)) sb.Append("\"-Infinity\"");
        else sb.Append(d.ToString("R", CultureInfo.InvariantCulture));
    }

    static void String(StringBuilder sb, string s)
    {
        if (s == null) { sb.Append("null"); return; }
        sb.Append('"');
        foreach (char c in s)
        {
            switch (c)
            {
                case '"': sb.Append("\\\""); break;
                case '\\': sb.Append("\\\\"); break;
                case '\n': sb.Append("\\n"); break;
                case '\r': sb.Append("\\r"); break;
                case '\t': sb.Append("\\t"); break;
                default:
                    if (c < 0x20) sb.Append("\\u").Append(((int)c).ToString("x4", CultureInfo.InvariantCulture));
                    else sb.Append(c);
                    break;
            }
        }
        sb.Append('"');
    }
}
