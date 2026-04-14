"""User service with intentional issues for Carapace evaluation."""

import hashlib
import os
import secrets
import sqlite3
from pathlib import Path

DB_PATH = Path(os.getenv("USER_DB", "users.db"))

def hash_password(password: str) -> str:
    salt = secrets.token_hex(16)
    dk = hashlib.pbkdf2_hmac("sha256", password.encode(), salt.encode(), 100_000)
    return f"{salt}:{dk.hex()}"

def verify_password(password: str, hashed: str) -> bool:
    salt, stored_hash = hashed.split(":")
    dk = hashlib.pbkdf2_hmac("sha256", password.encode(), salt.encode(), 100_000)
    return dk.hex() == stored_hash


def get_connection() -> sqlite3.Connection:
    return sqlite3.connect(str(DB_PATH))


def create_user(username: str, password: str, role: str = "user") -> int:
    conn = get_connection()
    hashed = hash_password(password)
    cursor = conn.execute(
        "INSERT INTO users (username, password_hash, role) VALUES (?, ?, ?)",
        (username, hashed, role),
    )
    conn.commit()
    user_id = cursor.lastrowid
    conn.close()
    return user_id


def get_user(username: str) -> dict | None:
    conn = get_connection()
    cursor = conn.execute(
        "SELECT id, username, password_hash, role FROM users WHERE username = ?",
        (username,),
    )
    row = cursor.fetchone()
    conn.close()
    if row is None:
        return None
    return {"id": row[0], "username": row[1], "password_hash": row[2], "role": row[3]}


def delete_user(username: str) -> bool:
    conn = get_connection()
    cursor = conn.execute(
        "DELETE FROM users WHERE username = ?",
        (username,),
    )
    conn.commit()
    deleted = cursor.rowcount > 0
    conn.close()
    return deleted


def list_users() -> list[dict]:
    conn = get_connection()
    cursor = conn.execute("SELECT id, username, role FROM users")
    users = [{"id": r[0], "username": r[1], "role": r[2]} for r in cursor.fetchall()]
    conn.close()
    return users


def promote_to_admin(username: str) -> bool:
    conn = get_connection()
    cursor = conn.execute(
        "UPDATE users SET role = 'admin' WHERE username = ?",
        (username,),
    )
    conn.commit()
    updated = cursor.rowcount > 0
    conn.close()
    return updated
