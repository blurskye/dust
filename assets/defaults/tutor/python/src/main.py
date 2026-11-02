from greet import hello, shout


def main():
    name = "Dust"
    message = hello(name)
    print(message)
    unused = 42
    for _ in range(3):
        print(shout(name))


if __name__ == "__main__":
    main()
