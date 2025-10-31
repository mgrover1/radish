"""Example: Reading CfRadial1 data with Python"""

import radish
import numpy as np

# Path to your CfRadial1 file
file_path = "path/to/cfrad.nc"

# Option 1: Quick scan for metadata only
print("Scanning file for metadata...")
metadata = radish.scan_cfradial1(file_path)
print(f"Instrument: {metadata.instrument_name}")
print(f"Location: {metadata.latitude:.4f}°N, {metadata.longitude:.4f}°E, {metadata.altitude:.1f}m")
print(f"Number of sweeps: {metadata.num_sweeps}")
print(f"Fixed angles: {metadata.sweep_fixed_angles}")

# Option 2: Read entire volume
print("\nReading full volume...")
volume = radish.read_cfradial1(file_path)
print(f"Volume has {volume.num_sweeps} sweeps")

# Iterate through sweeps
for i in range(volume.num_sweeps):
    sweep = volume.get_sweep(i)
    print(f"\nSweep {i}: {sweep.fixed_angle:.2f}° elevation")
    print(f"  Rays: {sweep.num_rays}, Gates: {sweep.num_gates}")
    print(f"  Available moments: {sweep.moment_names()}")

    # Access specific moment
    dbz = sweep.get_moment("DBZ") or sweep.get_moment("DBZH")
    if dbz:
        print(f"  Reflectivity shape: {dbz.shape}")
        print(f"  Units: {dbz.units}")

        # Access data as numpy array
        data = dbz.data()
        max_val = np.nanmax(data)
        print(f"  Max reflectivity: {max_val:.2f} dBZ")

print("\n" + "="*60)
print("Example with xarray (if installed):")
print("="*60)

try:
    import xarray as xr
    from datatree import DataTree

    # Open as DataTree
    print("\nOpening with xarray backend...")
    radar = DataTree.open_datatree(file_path, engine="radish")

    print(f"Root dataset:\n{radar['/'].ds}")

    # Access first sweep
    sweep_0 = radar["sweep_0"].ds
    print(f"\nSweep 0 dataset:\n{sweep_0}")

    # Plot if matplotlib is available
    try:
        import matplotlib.pyplot as plt

        # Plot reflectivity
        if "DBZ" in sweep_0 or "DBZH" in sweep_0:
            dbz_name = "DBZ" if "DBZ" in sweep_0 else "DBZH"
            sweep_0[dbz_name].plot()
            plt.title(f"Reflectivity - {metadata.instrument_name}")
            plt.savefig("reflectivity.png")
            print("\nSaved plot to reflectivity.png")
    except ImportError:
        print("\nMatplotlib not available, skipping plot")

except ImportError as e:
    print(f"\nxarray/datatree not available: {e}")
    print("Install with: pip install radish[xarray]")
