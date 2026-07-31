package main

import "fmt"

type profile struct {
	Name string
}

func main() {
	value := profile{Name: "Ada"}
	completion := value.Name
	hoverTarget := sharedLabel()
	localRenameTarget := "local"
	definitionUse := localRenameTarget
	diagnostic := value.Missing

	fmt.Println(completion, hoverTarget, definitionUse, diagnostic)
}
