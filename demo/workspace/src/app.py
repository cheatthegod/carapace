import os


def load_runtime_settings() -> dict[str, str]:
    return {
        "api_base_url": os.getenv("API_BASE_URL", "https://api.example.test"),
        "feature_flag_safe_mode": os.getenv("FEATURE_FLAG_SAFE_MODE", "true"),
    }


def format_status() -> str:
    """Return a human-readable summary of the current runtime configuration."""
    settings = load_runtime_settings()
    api_url = settings["api_base_url"]
    safe_mode_enabled = settings["feature_flag_safe_mode"] == "true"
    mode_label = "safe mode ON" if safe_mode_enabled else "safe mode OFF"
    return f"Demo app → {api_url} ({mode_label})"


if __name__ == "__main__":
    print(format_status())
