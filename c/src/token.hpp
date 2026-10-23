#pragma once

#include <cstdint>
#include <string>

enum class TokenKind {
    Return, Integer, Plus, Minus, Star, Slash, LeftParen, RightParen,
    Semicolon, End
};

struct Token {
    TokenKind kind;
    std::string text;
    std::int64_t integer = 0;
    std::size_t line = 1;
    std::size_t column = 1;
};

const char* token_name(TokenKind kind);
