import os


def load_runtime_settings() -> dict[str, str]:
    return {
        "api_base_url": os.getenv("API_BASE_URL", "https://api.example.test"),
        "feature_flag_safe_mode": os.getenv("FEATURE_FLAG_SAFE_MODE", "true"),
    }


def format_status() -> str:
    """Return a one-line status string showing the API target and safe-mode state."""
    settings = load_runtime_settings()
    api_url = settings["api_base_url"]
    safe_mode = settings["feature_flag_safe_mode"].lower() == "true"
    return f"Demo app → {api_url} (safe mode {'ON' if safe_mode else 'OFF'})"


if __name__ == "__main__":
    print(format_status())
