using System.Collections.Generic;
using Godot;

namespace SimWorld;

/// <summary>
/// The dither variants of one material for the tiles that are in a LOD fade. A fade needs a value per tile (<c>fade</c>, <c>fade_parity</c>, <c>fade_mul</c>). Those
/// used to be <c>instance uniform</c>s; the RenderingServer reserves 16 entries of one shared buffer (4096 nodes at the default
/// <c>rendering/limits/global_shader_variables/buffer_size</c>) for every node that ever gets a material declaring one, never gives them back before the node is freed,
/// and reads the instance uniforms of a node without a slot as <b>0</b>: after a long flight (pool of ~900 tile sets x 7 nodes, plus the terrain) about half of the pool
/// had no slot, so a faded tile of such a set drew no pixel (a tile-shaped hole for the whole swap) and its light sprites were black. Now a fading tile rents one
/// clone of the variant per node it dithers and writes plain uniforms into it; at most <c>fades.max_active</c> swaps fade at once, so a few dozen clones exist.
/// <para>The template is the variant that was configured at creation. Clones are made with <see cref="Resource.Duplicate"/> (parameters and textures copied, the shader
/// shared) and every later parameter change must go through <see cref="Set"/>, which writes the template and every clone, so no clone is stale.</para>
/// </summary>
public sealed class FadeMaterials
{
    private readonly ShaderMaterial _template;
    private readonly List<ShaderMaterial> _all = new();
    private readonly Stack<ShaderMaterial> _free = new();

    public FadeMaterials(ShaderMaterial template)
    {
        _template = template;
        _all.Add(template);
    }

    /// <summary>The configured variant (assigned to no tile node; <see cref="MaterialPrewarm"/> draws it for a few frames at start to compile its shader); read parameters from here.</summary>
    public ShaderMaterial Template => _template;
    /// <summary>Clones that exist (in use plus free).</summary>
    public int Clones => _all.Count - 1;
    public int FreeClones => _free.Count;

    /// <summary>A clone for one fading node.</summary>
    public ShaderMaterial Rent()
    {
        if (_free.TryPop(out var m)) return m;
        m = (ShaderMaterial)_template.Duplicate();
        _all.Add(m);
        return m;
    }

    public void Return(ShaderMaterial m) => _free.Push(m);

    /// <summary>Writes a parameter into the template and every clone.</summary>
    public void Set(StringName name, Variant value)
    {
        for (int i = 0; i < _all.Count; i++) _all[i].SetShaderParameter(name, value);
    }
}
