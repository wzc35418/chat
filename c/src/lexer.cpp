#include "lexer.hpp"

#include <charconv>
#include <cctype>
#include <stdexcept>
#include <system_error>

const char* token_name(TokenKind kind) {
    switch (kind) {
    case TokenKind::Return: return "return";
    case TokenKind::Integer: return "integer";
    case TokenKind::Plus: return "+";
    case TokenKind::Minus: return "-";
    case TokenKind::Star: return "*";
    case TokenKind::Slash: return "/";
    case TokenKind::LeftParen: return "(";
    case TokenKind::RightParen: return ")";
    case TokenKind::Semicolon: return ";";
    case TokenKind::End: return "end of file";
    }
    return "unknown";
}

std::vector<Token> lex(const std::string& source, const std::string& filename) {
    std::vector<Token> tokens;
    std::size_t offset = 0;
    std::size_t line = 1;
    std::size_t column = 1;

    while (offset < source.size()) {
        const unsigned char ch = static_cast<unsigned char>(source[offset]);
        if (ch == '\n') {
            ++offset;
            ++line;
            column = 1;
            continue;
        }
        if (std::isspace(ch)) {
            ++offset;
            ++column;
            continue;
        }

        const std::size_t start = offset;
        const std::size_t start_column = column;
        if (std::isdigit(ch)) {
            while (offset < source.size() &&
                   std::isdigit(static_cast<unsigned char>(source[offset]))) {
                ++offset;
                ++column;
            }
            const std::string text = source.substr(start, offset - start);
            std::int64_t value = 0;
            const auto result = std::from_chars(text.data(), text.data() + text.size(), value);
            if (result.ec != std::errc{} || result.ptr != text.data() + text.size()) {
                throw std::runtime_error(filename + ":" + std::to_string(line) + ":" +
                    std::to_string(start_column) + ": integer is out of range");
            }
            tokens.push_back({TokenKind::Integer, text, value, line, start_column});
            continue;
        }
        if (std::isalpha(ch) || ch == '_') {
            while (offset < source.size()) {
                const unsigned char next = static_cast<unsigned char>(source[offset]);
                if (!std::isalnum(next) && next != '_') break;
                ++offset;
                ++column;
            }
            const std::string text = source.substr(start, offset - start);
            if (text != "return") {
                throw std::runtime_error(filename + ":" + std::to_string(line) + ":" +
                    std::to_string(start_column) + ": unexpected word '" + text + "'");
            }
            tokens.push_back({TokenKind::Return, text, 0, line, start_column});
            continue;
        }

        TokenKind kind;
        switch (ch) {
        case '+': kind = TokenKind::Plus; break;
        case '-': kind = TokenKind::Minus; break;
        case '*': kind = TokenKind::Star; break;
        case '/': kind = TokenKind::Slash; break;
        case '(': kind = TokenKind::LeftParen; break;
        case ')': kind = TokenKind::RightParen; break;
        case ';': kind = TokenKind::Semicolon; break;
        default:
            throw std::runtime_error(filename + ":" + std::to_string(line) + ":" +
                std::to_string(column) + ": unexpected character '" + source[offset] + "'");
        }
        tokens.push_back({kind, source.substr(offset, 1), 0, line, column});
        ++offset;
        ++column;
    }

    tokens.push_back({TokenKind::End, "", 0, line, column});
    return tokens;
}
