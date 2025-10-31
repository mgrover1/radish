"""Tests for radish Python bindings"""

import pytest
import numpy as np

# Skip all tests if radish is not installed
pytest.importorskip("radish")

import radish


def test_import():
    """Test that radish can be imported"""
    assert hasattr(radish, "VolumeData")
    assert hasattr(radish, "VolumeMetadata")
    assert hasattr(radish, "SweepData")
    assert hasattr(radish, "MomentData")
    assert hasattr(radish, "read_cfradial1")
    assert hasattr(radish, "scan_cfradial1")


def test_version():
    """Test that version is defined"""
    assert hasattr(radish, "__version__")
    assert isinstance(radish.__version__, str)


# Note: The following tests require actual CfRadial1 test data
# They are marked as skip until test data is available

@pytest.mark.skip(reason="Requires test data")
def test_read_cfradial1():
    """Test reading a CfRadial1 file"""
    volume = radish.read_cfradial1("tests/data/test.nc")

    assert isinstance(volume, radish.VolumeData)
    assert volume.num_sweeps > 0

    metadata = volume.metadata
    assert isinstance(metadata, radish.VolumeMetadata)
    assert isinstance(metadata.instrument_name, str)
    assert isinstance(metadata.latitude, float)
    assert isinstance(metadata.longitude, float)


@pytest.mark.skip(reason="Requires test data")
def test_scan_cfradial1():
    """Test scanning a CfRadial1 file for metadata"""
    metadata = radish.scan_cfradial1("tests/data/test.nc")

    assert isinstance(metadata, radish.VolumeMetadata)
    assert metadata.num_sweeps > 0


@pytest.mark.skip(reason="Requires test data")
def test_sweep_access():
    """Test accessing sweep data"""
    volume = radish.read_cfradial1("tests/data/test.nc")
    sweep = volume.get_sweep(0)

    assert isinstance(sweep, radish.SweepData)
    assert sweep.num_rays > 0
    assert sweep.num_gates > 0
    assert len(sweep.moment_names()) > 0


@pytest.mark.skip(reason="Requires test data")
def test_moment_access():
    """Test accessing moment data"""
    volume = radish.read_cfradial1("tests/data/test.nc")
    sweep = volume.get_sweep(0)

    moment_names = sweep.moment_names()
    assert len(moment_names) > 0

    moment = sweep.get_moment(moment_names[0])
    assert isinstance(moment, radish.MomentData)
    assert isinstance(moment.name, str)
    assert isinstance(moment.units, str)

    # Test data access
    data = moment.data()
    assert isinstance(data, np.ndarray)
    assert data.ndim == 2


@pytest.mark.skip(reason="Requires test data and xarray")
def test_xarray_backend():
    """Test xarray backend integration"""
    from datatree import DataTree

    radar = DataTree.open_datatree("tests/data/test.nc", engine="radish")

    assert "/" in radar
    assert "sweep_0" in radar

    sweep_0 = radar["sweep_0"].ds
    assert "azimuth" in sweep_0.coords
    assert "elevation" in sweep_0.coords
    assert "range" in sweep_0.coords
