using System;

namespace SimWorld;

// Debug taps for host-side checks (the bench's --hole-check): observe vector tile uploads and unloads without touching the cover.
public partial class WorldView
{
    /// <summary>Called after a vector tile was uploaded (nodes, true = in-place re-delivery of a held key). Debug tap; null = off.</summary>
    public Action<TileNodes, bool>? VectorTileUploaded;
    /// <summary>Called before a vector key is released through poll_unload (key, its nodes or null when the host did not hold it). Debug tap; null = off.</summary>
    public Action<string, TileNodes?>? VectorTileUnloading;
}
