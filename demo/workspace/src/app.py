import os


def load_runtime_settings() -> dict[str, str]:
    return {
        "api_base_url": os.getenv("API_BASE_URL", "https://api.example.test"),
        "feature_flag_safe_mode": os.getenv("FEATURE_FLAG_SAFE_MODE", "true"),
    }


def format_status() -> str:
    settings = load_runtime_settings()
    return (
        "Demo app configured for "
        f"{settings['api_base_url']} "
        f"(safe_mode={settings['feature_flag_safe_mode']})"
    )


if __name__ == "__main__":
    print(format_status())
