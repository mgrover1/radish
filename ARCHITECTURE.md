# Radish Architecture

A high-performance weather radar data library with a Rust core and Python bindings, designed for reading multiple radar formats and normalizing to the CfRadial2/FM301 standard.

## Architecture Overview

```mermaid
graph TB
    subgraph "User Layer"
        A[Python User Code]
        B[xarray/datatree]
    end

    subgraph "Python Bindings Layer"
        C[PyO3 Bindings]
        D[xarray Backend Entry Point]
        E[Python Accessors]
    end

    subgraph "Rust Core"
        F[Backend Trait]
        G[CfRadial1 Backend]
        H[CfRadial2 Backend]
        I[IRIS Backend]
        J[NEXRAD Backend]
        K[ODIM Backend]

        L[Data Model]
        M[VolumeData]
        N[SweepData]
        O[MomentData]

        P[I/O Utilities]
        Q[HDF5 Reader]
        R[Binary Parser]

        S[Transforms]
        T[Georeference]
        U[QC/Filters]
    end

    A --> B
    B --> D
    D --> C
    C --> F
    F --> G
    F --> H
    F --> I
    F --> J
    F --> K

    G --> L
    H --> L
    I --> L
    J --> L
    K --> L

    L --> M
    M --> N
    N --> O

    G --> Q
    H --> Q
    I --> R
    J --> R
    K --> Q

    M --> S
    S --> T
    S --> U
```

## Data Flow Architecture

```mermaid
sequenceDiagram
    participant User
    participant xarray
    participant Backend Entry Point
    participant PyO3 Bindings
    participant Rust Backend
    participant HDF5/Binary Parser
    participant Data Model

    User->>xarray: open_datatree("file.nc", engine="radish")
    xarray->>Backend Entry Point: open_dataset()
    Backend Entry Point->>PyO3 Bindings: read_volume_py()
    PyO3 Bindings->>Rust Backend: read_volume()
    Rust Backend->>HDF5/Binary Parser: scan_file()
    HDF5/Binary Parser-->>Rust Backend: file structure
    Rust Backend->>HDF5/Binary Parser: read_sweep(i)
    HDF5/Binary Parser-->>Rust Backend: raw data
    Rust Backend->>Data Model: normalize to VolumeData
    Data Model-->>Rust Backend: VolumeData
    Rust Backend-->>PyO3 Bindings: VolumeData
    PyO3 Bindings-->>Backend Entry Point: DataTree
    Backend Entry Point-->>xarray: DataTree
    xarray-->>User: DataTree object
```

## Component Architecture

```mermaid
graph LR
    subgraph "Core Data Model"
        A[VolumeData]
        B[VolumeMetadata]
        C[SweepData]
        D[SweepMetadata]
        E[MomentData]
        F[Coordinates]
        G[RadarCalibration]
    end

    A --> B
    A --> C
    A --> G
    C --> D
    C --> E
    C --> F

    subgraph "Backend System"
        H[Backend Trait]
        I[scan_file]
        J[read_sweep]
        K[read_volume]
    end

    H --> I
    H --> J
    H --> K

    K --> A
    J --> C
```

## Module Structure

```mermaid
graph TB
    subgraph "radish/ (Core Crate)"
        A[lib.rs]
        B[error.rs]
        C[model/]
        D[backends/]
        E[io/]
        F[transforms/]
    end

    subgraph "python/ (PyO3 Crate)"
        G[lib.rs - PyModule]
        H[datatree.rs]
        I[backends.rs]
        J[radish/__init__.py]
        K[radish/backends/]
    end

    subgraph "types/ (Shared Crate)"
        L[common types]
        M[constants]
    end

    A --> B
    A --> C
    A --> D
    A --> E
    A --> F

    G --> A
    G --> H
    G --> I
    H --> C
    I --> D

    C --> L
    D --> L
```

## Backend Implementation Pattern

Each backend follows this pattern:

1. **File Scanning**: Quick metadata extraction without loading full data
2. **Sweep Reading**: Load individual sweep data on demand
3. **Normalization**: Convert to standard VolumeData/SweepData model
4. **Validation**: Ensure data meets CfRadial2 requirements

```rust
pub trait RadarBackend: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn supported_extensions(&self) -> &[&str];

    fn scan_file(&self, path: &Path) -> Result<VolumeMetadata>;
    fn read_sweep(&self, path: &Path, sweep_idx: usize) -> Result<SweepData>;
    fn read_volume(&self, path: &Path) -> Result<VolumeData>;
}
```

## Data Model Hierarchy

```mermaid
classDiagram
    class VolumeData {
        +VolumeMetadata metadata
        +Vec~SweepData~ sweeps
        +Option~RadarCalibration~ calibration
        +get_sweep(idx) SweepData
        +filter_moments(names) Self
    }

    class VolumeMetadata {
        +String instrument_name
        +String institution
        +f64 latitude
        +f64 longitude
        +f64 altitude
        +DateTime time_coverage_start
        +DateTime time_coverage_end
        +Vec~String~ sweep_group_names
        +Vec~f64~ sweep_fixed_angles
    }

    class SweepData {
        +u32 sweep_number
        +SweepMode sweep_mode
        +HashMap~String, MomentData~ moments
        +Coordinates coordinates
        +get_moment(name) Option~MomentData~
    }

    class MomentData {
        +String name
        +String standard_name
        +String units
        +Array2~f32~ data
        +Option~f32~ fill_value
        +Option~f32~ scale_factor
    }

    class Coordinates {
        +Vec~f64~ time
        +Vec~f32~ range
        +Vec~f32~ azimuth
        +Vec~f32~ elevation
    }

    VolumeData --> VolumeMetadata
    VolumeData --> SweepData
    SweepData --> MomentData
    SweepData --> Coordinates
```

## Performance Considerations

1. **Zero-Copy Where Possible**: Use memory-mapped files for large datasets
2. **Lazy Loading**: Read sweeps on demand, not all at once
3. **Parallel Processing**: Use rayon for multi-threaded sweep processing
4. **Efficient Memory Layout**: Use ndarray for numerical data
5. **Minimal Python Overhead**: Keep hot paths in Rust

## Extension Points

1. **New Backends**: Implement `RadarBackend` trait
2. **New Transforms**: Add to `transforms/` module
3. **Custom Moments**: Extend `MomentData` types
4. **Export Formats**: Add writers alongside readers
5. **Compression**: Plug in via I/O layer

## Technology Stack

### Rust Core
- **hdf5**: HDF5 file format support
- **netcdf**: NetCDF support (CfRadial uses NetCDF-4)
- **ndarray**: Multi-dimensional arrays
- **chrono**: Date/time handling
- **thiserror**: Error handling
- **rayon**: Parallel processing
- **serde**: Serialization

### Python Bindings
- **pyo3**: Rust-Python bindings
- **numpy**: Array interop
- **maturin**: Build system
- **xarray**: Data model integration
- **datatree**: Hierarchical data structure

## Development Roadmap

### Phase 1: Foundation (Current)
- [x] Project structure
- [ ] Core data model
- [ ] Backend trait
- [ ] CfRadial1 backend
- [ ] Basic Python bindings
- [ ] xarray integration

### Phase 2: Format Support
- [ ] CfRadial2 backend
- [ ] ODIM H5 backend
- [ ] IRIS/Sigmet backend
- [ ] NEXRAD Level 2 backend

### Phase 3: Advanced Features
- [ ] Georeferencing
- [ ] Velocity dealiasing
- [ ] Quality control filters
- [ ] Format conversion/export

### Phase 4: Optimization
- [ ] Memory-mapped I/O
- [ ] Parallel sweep loading
- [ ] Streaming API
- [ ] Compression support
