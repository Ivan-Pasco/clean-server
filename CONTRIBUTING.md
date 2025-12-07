# Contributing to Clean Server

Thank you for your interest in contributing to Clean Server! This document provides guidelines and information for contributors.

## Code of Conduct

We are committed to providing a welcoming and inspiring community for all. Please be respectful and constructive in your interactions.

## How to Contribute

### Reporting Bugs

If you find a bug, please create an issue with:

1. **Clear title**: Describe the issue in one line
2. **Description**: Detailed explanation of the bug
3. **Reproduction steps**: How to reproduce the issue
4. **Expected behavior**: What should happen
5. **Actual behavior**: What actually happens
6. **Environment**: OS, Rust version, Clean Server version
7. **Logs**: Relevant error messages or logs

### Suggesting Enhancements

For feature requests:

1. **Use case**: Explain why this feature is needed
2. **Proposed solution**: Describe your proposed implementation
3. **Alternatives**: Any alternative solutions you've considered
4. **Additional context**: Mockups, examples, etc.

### Pull Requests

1. **Fork the repository** and create your branch from `master`
2. **Make your changes** following our coding standards
3. **Add tests** for any new functionality
4. **Ensure tests pass**: Run `cargo test`
5. **Update documentation** if needed
6. **Write clear commit messages** using conventional commits
7. **Submit a pull request**

## Development Setup

### Prerequisites

- Rust 1.75 or later
- Git

### Building

```bash
git clone https://github.com/Ivan-Pasco/clean-server.git
cd clean-server
cargo build
```

### Running Tests

```bash
# Run all tests
cargo test

# Run with verbose output
cargo test -- --nocapture

# Run specific test
cargo test test_name
```

### Running the Server

```bash
# Build in debug mode
cargo run -- app.wasm

# Build in release mode
cargo run --release -- app.wasm

# With custom options
cargo run -- app.wasm --port 8080 --verbose
```

## Coding Standards

### Rust Style Guide

Follow the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/):

- Use `rustfmt` for formatting: `cargo fmt`
- Use `clippy` for linting: `cargo clippy`
- Write documentation comments for public APIs
- Use descriptive variable and function names

### Code Organization

- **src/main.rs**: CLI entry point
- **src/lib.rs**: Library exports
- **src/server.rs**: HTTP server implementation
- **src/router.rs**: Route handling
- **src/wasm.rs**: WASM runtime
- **src/bridge.rs**: Host Bridge integration
- **src/memory.rs**: Memory management
- **src/error.rs**: Error types
- **host-bridge/**: Host Bridge implementation

### Naming Conventions

- **Types**: `PascalCase`
- **Functions**: `snake_case`
- **Constants**: `SCREAMING_SNAKE_CASE`
- **Modules**: `snake_case`

### Error Handling

- Use `Result<T, RuntimeError>` for fallible operations
- Provide meaningful error messages
- Use the standard error envelope format
- Include context in error messages

### Testing

- Write unit tests for individual functions
- Write integration tests for end-to-end flows
- Use descriptive test names: `test_feature_succeeds_when_condition`
- Test both success and failure cases
- Use `#[should_panic]` for tests that expect panics

### Documentation

- Document all public APIs with `///` doc comments
- Include examples in documentation
- Explain the "why" not just the "what"
- Keep documentation up to date with code changes

## Commit Messages

We use [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <subject>

<body>

<footer>
```

**Types:**
- `feat`: New feature
- `fix`: Bug fix
- `docs`: Documentation changes
- `style`: Code style changes (formatting)
- `refactor`: Code refactoring
- `perf`: Performance improvements
- `test`: Adding or updating tests
- `chore`: Maintenance tasks

**Examples:**
```
feat(wasm): add support for WASM component model
fix(http): handle connection timeout errors
docs(readme): update installation instructions
refactor(router): simplify route matching logic
```

## Pull Request Process

1. **Create a branch**: Use a descriptive name
   - `feat/add-compression`
   - `fix/memory-leak`
   - `docs/api-reference`

2. **Make your changes**: Follow coding standards

3. **Write tests**: Ensure new code is tested

4. **Update documentation**: Keep docs in sync

5. **Run checks**:
   ```bash
   cargo fmt --check
   cargo clippy -- -D warnings
   cargo test
   ```

6. **Commit changes**: Use conventional commits

7. **Push to your fork**:
   ```bash
   git push origin feat/add-compression
   ```

8. **Create pull request**: Fill out the template

9. **Address feedback**: Respond to review comments

10. **Merge**: Once approved, maintainers will merge

## Review Process

All pull requests require:

- Passing CI checks
- Code review approval
- No merge conflicts
- Updated documentation
- Passing tests

Maintainers will review PRs within 7 days.

## Release Process

Releases follow semantic versioning (semver):

- **Major** (2.0.0): Breaking changes
- **Minor** (1.1.0): New features, backward compatible
- **Patch** (1.0.1): Bug fixes

Only maintainers create releases.

## Host Bridge Development

When contributing to the Host Bridge:

1. **Follow JSON envelope format**: All calls use standard envelope
2. **Handle errors properly**: Use standard error codes
3. **Write comprehensive tests**: Test all bridge functions
4. **Document security implications**: Note any security concerns
5. **Maintain backward compatibility**: Don't break existing contracts

### Adding New Bridge Functions

1. Add function to appropriate bridge module (http.rs, db.rs, etc.)
2. Update bridge dispatcher in lib.rs
3. Add tests
4. Document in code comments
5. Update README.md capabilities section

## Testing Strategy

### Unit Tests

Test individual components in isolation:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_router_match_exact_path() {
        let router = Router::new();
        router.add_route(HttpMethod::GET, "/users", 0);

        let result = router.match_route(HttpMethod::GET, "/users");
        assert!(result.is_some());
    }
}
```

### Integration Tests

Test end-to-end flows in `tests/`:

```rust
#[tokio::test]
async fn test_server_handles_request() {
    let server = start_test_server().await;
    let response = reqwest::get("http://localhost:3000/").await.unwrap();
    assert_eq!(response.status(), 200);
}
```

### Performance Tests

Use criterion for benchmarks:

```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn benchmark_router(c: &mut Criterion) {
    c.bench_function("route_match", |b| {
        b.iter(|| router.match_route(black_box(method), black_box(path)))
    });
}
```

## Security

### Reporting Security Issues

**DO NOT** create public issues for security vulnerabilities.

Email security concerns to: security@cleanframework.com

We will respond within 48 hours.

### Security Best Practices

- Validate all external input
- Use parameterized queries
- Escape HTML output
- Implement rate limiting
- Use secure defaults
- Follow principle of least privilege

## Getting Help

- **GitHub Issues**: For bugs and feature requests
- **GitHub Discussions**: For questions and discussions
- **Discord**: https://discord.gg/cleanlang
- **Email**: support@cleanframework.com

## License

By contributing, you agree that your contributions will be licensed under the same terms as the project (MIT OR Apache-2.0).

## Recognition

Contributors will be recognized in:

- CHANGELOG.md for each release
- README.md contributors section
- GitHub contributors page

Thank you for making Clean Server better!
